"""CloudDrop integration for Nautilus (Files).

Shows the upload state of files in the CloudDrop folder as emblems and in a
column named after the cloud provider ("Google Drive" unless config.toml
names another), and adds context-menu actions that run the ``clouddrop`` CLI.
State is read from the daemon's SQLite database, opened read-only; this
module never touches the network.

The first half of the file is plain Python with no Nautilus objects (database
access and decisions); the Nautilus glue is at the bottom.
"""

import collections
import functools
import os
import shutil
import sqlite3
import subprocess
import threading
import time
import tomllib
import traceback
from typing import NamedTuple, Optional
from urllib.parse import quote

import gi

gi.require_version('Nautilus', '4.1')
from gi.repository import GLib, GObject, Nautilus  # noqa: E402

# Emblem names. These exist in the Yaru (Ubuntu default) and HighContrast
# themes; emblem-synchronizing is also in Adwaita. Yaru has no
# "emblem-synchronized", so the green check (emblem-default) marks uploaded
# items.
EMBLEM_SYNCED = 'emblem-default'
EMBLEM_BUSY = 'emblem-synchronizing'
EMBLEM_ERROR = 'emblem-important'

STATUS_ATTRIBUTE = 'clouddrop_status'
POLL_INTERVAL_S = 2
POLL_BUDGET_S = 0.03     # max time one poll tick spends re-checking files
MAX_TRACKED = 4000       # annotated files remembered for live refresh
BUSY_TIMEOUT_S = 0.05    # never make the file manager wait on the daemon
CLOUD_MENU_LIMIT = 25
MAX_ERROR_LEN = 200

SQLITE_BUSY = 5
SQLITE_LOCKED = 6


# --------------------------------------------------------------------------
# Plain logic
# --------------------------------------------------------------------------

class Unavailable(Exception):
    """The state database cannot be read right now (missing, busy, broken)."""


class Entry(NamedTuple):
    kind: str
    state: str
    drive_id: Optional[str]
    error: Optional[str]


class Subtree(NamedTuple):
    error: bool
    uploading: bool
    pending: bool


class Status(NamedTuple):
    emblem: str
    label: str


NO_STATUS = Status('', '')
UPLOADED = Status(EMBLEM_SYNCED, 'Uploaded')
UPLOADING = Status(EMBLEM_BUSY, 'Uploading')
WAITING = Status(EMBLEM_BUSY, 'Waiting to upload')


class MenuSpec(NamedTuple):
    """A menu entry described without Nautilus objects."""
    name: str
    label: str
    tip: str = ''
    argv: Optional[tuple] = None
    sensitive: bool = True
    children: tuple = ()


def data_dir():
    override = os.environ.get('CLOUDDROP_DATA_DIR')
    if override:
        return override
    base = os.environ.get('XDG_DATA_HOME') or os.path.join(
        os.path.expanduser('~'), '.local', 'share')
    return os.path.join(base, 'clouddrop')


def db_path():
    return os.path.join(data_dir(), 'state.db')


def config_path():
    override = os.environ.get('CLOUDDROP_CONFIG')
    if override:
        return override
    base = os.environ.get('XDG_CONFIG_HOME') or os.path.join(
        os.path.expanduser('~'), '.config')
    return os.path.join(base, 'clouddrop', 'config.toml')


_keep_cache = (None, [])


def keep_local_folders():
    """Folders marked "always keep on this computer" (cached by file mtime)."""
    global _keep_cache
    path = config_path()
    try:
        key = (path, os.stat(path).st_mtime_ns)
    except OSError:
        return []
    if _keep_cache[0] == key:
        return _keep_cache[1]
    try:
        with open(path, 'rb') as f:
            data = tomllib.load(f)
        folders = [str(k).strip('/') for k in data.get('keep_local', [])]
    except (OSError, ValueError):
        folders = []
    _keep_cache = (key, folders)
    return folders


class Provider(NamedTuple):
    id: str
    name: str          # shown in menus and as the column title
    web_links: bool    # items have a web page, so "Open in …" makes sense


# Mirrors the id, name and web links of each provider's INFO in src/<id>.rs.
PROVIDERS = {p.id: p for p in (
    Provider('gdrive', 'Google Drive', True),
    Provider('onedrive', 'OneDrive', True),
    Provider('dropbox', 'Dropbox', True),
    Provider('nextcloud', 'Nextcloud', True),
    Provider('webdav', 'WebDAV', False),
    Provider('s3', 'S3', False),
    Provider('sftp', 'SFTP', False),
    Provider('mega', 'MEGA', False),
)}
GOOGLE_DRIVE = PROVIDERS['gdrive']

_provider_cache = (None, GOOGLE_DRIVE)


def configured_provider():
    """The provider config.toml names; Google Drive if it names none.

    Cached by file mtime. An unknown id is shown as itself, without web links.
    """
    global _provider_cache
    path = config_path()
    try:
        key = (path, os.stat(path).st_mtime_ns)
    except OSError:
        return GOOGLE_DRIVE
    if _provider_cache[0] == key:
        return _provider_cache[1]
    try:
        with open(path, 'rb') as f:
            value = tomllib.load(f).get('provider')
    except (OSError, ValueError):
        value = None
    if isinstance(value, str) and value:
        provider = PROVIDERS.get(value) or Provider(value, value, False)
    else:
        provider = GOOGLE_DRIVE
    _provider_cache = (key, provider)
    return provider


def _is_busy(exc):
    code = getattr(exc, 'sqlite_errorcode', None)
    if code is not None:
        return (code & 0xFF) in (SQLITE_BUSY, SQLITE_LOCKED)
    text = str(exc).lower()
    return 'locked' in text or 'busy' in text


def _subtree_clause(rel):
    """SQL selecting the descendants of directory `rel` ('' = everything).

    'dir/' <= path < 'dir0' is an index range: '0' is the character after '/'.
    """
    if not rel:
        return '1', ()
    return 'path >= ? AND path < ?', (rel + '/', rel + '0')


def relative_path(root, path):
    """Path relative to the sync root: '' for the root, None if outside."""
    path = os.path.normpath(path)
    if path == root:
        return ''
    prefix = root if root.endswith('/') else root + '/'
    if path.startswith(prefix):
        return path[len(prefix):]
    return None


class StateDB:
    """Read-only, non-blocking view of the CloudDrop state database."""

    def __init__(self):
        self._conn = None
        self._ident = None
        self.generation = 0   # bumped on every (re)connect

    def close(self):
        conn, self._conn, self._ident = self._conn, None, None
        if conn is not None:
            try:
                conn.close()
            except sqlite3.Error:
                pass

    def _connection(self):
        path = db_path()
        try:
            st = os.stat(path)
        except OSError:
            self.close()
            raise Unavailable('no database')
        ident = (path, st.st_dev, st.st_ino)
        if self._conn is not None and ident == self._ident:
            return self._conn
        # First use, or the file was replaced: start over.
        self.close()
        conn = None
        try:
            conn = sqlite3.connect('file:%s?mode=ro' % quote(path), uri=True,
                                   timeout=BUSY_TIMEOUT_S)
            conn.execute('PRAGMA data_version').fetchall()
        except sqlite3.Error as exc:
            if conn is not None:
                conn.close()
            raise Unavailable(str(exc))
        self._conn, self._ident = conn, ident
        self.generation += 1
        return conn

    def _query(self, sql, params=()):
        conn = self._connection()
        try:
            # fetchall() so no statement stays open: an open read transaction
            # would freeze PRAGMA data_version and block WAL checkpoints.
            return conn.execute(sql, params).fetchall()
        except sqlite3.Error as exc:
            if not _is_busy(exc):
                self.close()
            raise Unavailable(str(exc))

    def data_version(self):
        return self._query('PRAGMA data_version')[0][0]

    def meta(self, key):
        rows = self._query('SELECT value FROM meta WHERE key = ?', (key,))
        return rows[0][0] if rows else None

    def sync_root(self):
        value = self.meta('sync_root')
        if not value:
            raise Unavailable('no sync_root')
        return os.path.normpath(value)

    def entry(self, rel):
        rows = self._query(
            'SELECT kind, state, drive_id, error FROM entries WHERE path = ?',
            (rel,))
        return Entry(*rows[0]) if rows else None

    def drive_id(self, rel):
        """Drive id of an entry; the sync root maps to meta 'root_id'."""
        if not rel:
            return self.meta('root_id') or None
        entry = self.entry(rel)
        return entry.drive_id if entry and entry.drive_id else None

    def subtree(self, rel):
        clause, params = _subtree_clause(rel)
        row = self._query(
            "SELECT COALESCE(MAX(state = 'error'), 0),"
            " COALESCE(MAX(state = 'uploading'), 0),"
            " COALESCE(MAX(state = 'pending'), 0)"
            ' FROM entries WHERE ' + clause, params)[0]
        return Subtree(*(bool(v) for v in row))

    def cloud_children(self, rel, limit):
        """Direct cloud_only children of `rel`: (total, first `limit` sorted).

        Each child is (name, is_dir); directories sort first, then by name.
        """
        prefix = rel + '/' if rel else ''
        clause, params = _subtree_clause(rel)
        rows = self._query(
            'SELECT path, kind FROM entries WHERE ' + clause +
            " AND state = 'cloud_only' AND instr(substr(path, ?), '/') = 0",
            params + (len(prefix) + 1,))
        children = [(path[len(prefix):], kind == 'dir') for path, kind in rows]
        children.sort(key=lambda c: (not c[1], c[0].casefold(), c[0]))
        return len(children), children[:limit]


def _failed(message):
    message = ' '.join((message or '').split())
    if len(message) > MAX_ERROR_LEN:
        message = message[:MAX_ERROR_LEN - 1] + '…'
    return Status(EMBLEM_ERROR,
                  'Upload failed: ' + message if message else 'Upload failed')


def compute_status(db, rel, is_dir):
    """Status of a path below the sync root. Raises Unavailable."""
    entry = db.entry(rel)
    if not is_dir:
        if entry is None:
            return NO_STATUS
        if entry.state == 'synced':
            return UPLOADED
        if entry.state == 'uploading':
            return UPLOADING
        if entry.state == 'pending':
            return WAITING
        if entry.state == 'error':
            return _failed(entry.error)
        return NO_STATUS  # cloud_only: the daemon has not noticed the file yet

    sub = db.subtree(rel)
    state = entry.state if entry else None
    if state == 'error':
        return _failed(entry.error)
    if sub.error:
        return _failed(None)
    if state == 'uploading' or sub.uploading:
        return UPLOADING
    if state == 'pending' or sub.pending:
        return WAITING
    if state == 'synced':
        return UPLOADED
    return NO_STATUS


def find_binary():
    found = shutil.which('clouddrop')
    if found:
        return found
    home = os.path.expanduser('~')
    for candidate in (os.path.join(home, '.local', 'bin', 'clouddrop'),
                      os.path.join(home, '.cargo', 'bin', 'clouddrop')):
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def plan_file_menu(db, selection, binary, provider=GOOGLE_DRIVE):
    """Menu for selected items; selection is [(abs_path, rel, is_dir)]."""
    free, retry = [], []
    for path, rel, is_dir in selection:
        entry = db.entry(rel) if rel else None
        if is_dir or (entry and entry.state == 'synced'):
            free.append(path)
        if (entry and entry.state == 'error') or (
                is_dir and db.subtree(rel).error):
            retry.append(path)

    specs = []
    if free:
        specs.append(MenuSpec(
            'CloudDrop::free', 'Free Up Space',
            'Remove local copies that are safely stored in %s' % provider.name,
            (binary, '--notify', 'free', *free)))
    if (provider.web_links and len(selection) == 1
            and db.drive_id(selection[0][1])):
        specs.append(MenuSpec(
            'CloudDrop::open', 'Open in %s' % provider.name,
            'Open this item in %s' % provider.name,
            (binary, 'open', selection[0][0])))
    if retry:
        specs.append(MenuSpec(
            'CloudDrop::retry', 'Retry Upload',
            'Queue the failed uploads again',
            (binary, '--notify', 'retry', *retry)))
    if len(selection) == 1 and selection[0][1] and selection[0][2]:
        path, rel, _is_dir = selection[0]
        folders = keep_local_folders()
        if rel in folders:
            specs.append(MenuSpec(
                'CloudDrop::unkeep', 'Allow Automatic Free Up Space',
                'Let CloudDrop free this folder automatically again',
                (binary, '--notify', 'unkeep', path)))
        elif not any(rel.startswith(k + '/') for k in folders):
            specs.append(MenuSpec(
                'CloudDrop::keep', 'Always Keep on This Computer',
                'Automatic space saving will never free files in this folder',
                (binary, '--notify', 'keep', path)))
    return specs


def plan_background_menu(db, folder, rel, binary, provider=GOOGLE_DRIVE):
    """Menu for empty space in `folder` (absolute path, `rel` inside root)."""
    specs = []
    total, shown = db.cloud_children(rel, CLOUD_MENU_LIMIT)
    if total:
        children = [MenuSpec(
            'CloudDrop::restore_all', 'Download All',
            'Download every cloud-only item in this folder',
            (binary, '--notify', 'restore', folder))]
        for index, (name, _is_dir) in enumerate(shown):
            children.append(MenuSpec(
                'CloudDrop::restore_%d' % index, name,
                'Download %s from %s' % (name, provider.name),
                (binary, '--notify', 'restore', os.path.join(folder, name))))
        if total > len(shown):
            children.append(MenuSpec(
                'CloudDrop::restore_more',
                '…and %d more' % (total - len(shown)),
                sensitive=False))
        specs.append(MenuSpec(
            'CloudDrop::cloud_only', 'Cloud-only Files (%d)' % total,
            'Files kept in %s but removed from this computer' % provider.name,
            children=tuple(children)))
    if provider.web_links and db.drive_id(rel):
        specs.append(MenuSpec(
            'CloudDrop::open_folder', 'Open Folder in %s' % provider.name,
            'Open this folder in %s' % provider.name,
            (binary, 'open', folder)))
    return specs


def launch(argv):
    """Run a command detached; never blocks and never raises."""
    try:
        proc = subprocess.Popen(
            list(argv), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True)
    except OSError:
        return
    threading.Thread(target=proc.wait, daemon=True).start()  # reap


# --------------------------------------------------------------------------
# Nautilus glue
# --------------------------------------------------------------------------

def _guarded(fallback):
    """Keep exceptions from escaping into Nautilus."""
    def decorate(func):
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            try:
                return func(*args, **kwargs)
            except Exception:
                if os.environ.get('CLOUDDROP_DEBUG'):
                    traceback.print_exc()
                return fallback()
        return wrapper
    return decorate


def local_path(file):
    """Local filesystem path of a NautilusFileInfo, or None."""
    if file.get_uri_scheme() != 'file':
        return None
    location = file.get_location()
    return location.get_path() if location is not None else None


class Tracked(NamedTuple):
    file: object
    is_dir: bool
    status: Optional[Status]   # None: could not be determined


class CloudDropExtension(GObject.GObject, Nautilus.InfoProvider,
                         Nautilus.MenuProvider, Nautilus.ColumnProvider):

    def __init__(self):
        super().__init__()
        self._db = StateDB()
        self._seen = collections.OrderedDict()   # path -> Tracked, LRU order
        self._queue = []                          # paths awaiting a re-check
        self._token = None                        # (generation, data_version)
        GLib.timeout_add_seconds(POLL_INTERVAL_S, self._on_tick)

    # -- ColumnProvider ----------------------------------------------------

    def get_columns(self):
        # Asked once at startup, so a provider switch shows after a restart.
        name = configured_provider().name
        return [Nautilus.Column(
            name='CloudDrop::status', attribute=STATUS_ATTRIBUTE,
            label=name, description='Upload status in %s' % name)]

    # -- InfoProvider ------------------------------------------------------

    @_guarded(lambda: None)
    def update_file_info(self, file):
        path = local_path(file)
        if path is None:
            return
        try:
            root = self._db.sync_root()
        except Unavailable:
            return
        rel = relative_path(root, path)
        if not rel:
            return
        is_dir = file.is_directory()
        try:
            status = compute_status(self._db, rel, is_dir)
        except Unavailable:
            status = None
        self._remember(path, Tracked(file, is_dir, status))
        if status is not None:
            file.add_string_attribute(STATUS_ATTRIBUTE, status.label)
            if status.emblem:
                file.add_emblem(status.emblem)

    def _remember(self, path, tracked):
        self._seen[path] = tracked
        self._seen.move_to_end(path)
        while len(self._seen) > MAX_TRACKED:
            self._seen.popitem(last=False)

    # -- live refresh ------------------------------------------------------

    @_guarded(lambda: True)
    def _on_tick(self):
        self.poll()
        return True   # keep the timer running

    def poll(self):
        """Re-check remembered files if the database changed since last time.

        Work is bounded by POLL_BUDGET_S per call; leftovers carry over to the
        next call.
        """
        if not self._seen and not self._queue:
            return
        try:
            version = self._db.data_version()
            root = self._db.sync_root()
        except Unavailable:
            return
        token = (self._db.generation, version)
        if token != self._token:
            self._token = token
            self._queue = list(self._seen)   # newest annotated is popped first
        deadline = time.monotonic() + POLL_BUDGET_S
        while self._queue:
            self._refresh(self._queue.pop(), root)
            if time.monotonic() >= deadline:
                break

    def _refresh(self, path, root):
        tracked = self._seen.get(path)
        if tracked is None:
            return
        rel = relative_path(root, path)
        if not rel or tracked.file.is_gone():
            del self._seen[path]
            return
        try:
            status = compute_status(self._db, rel, tracked.is_dir)
        except Unavailable:
            return
        if status != tracked.status:
            self._seen[path] = tracked._replace(status=status)
            tracked.file.invalidate_extension_info()

    # -- MenuProvider ------------------------------------------------------

    @_guarded(list)
    def get_file_items(self, files):
        if not files:
            return []
        try:
            root = self._db.sync_root()
            selection = []
            for file in files:
                path = local_path(file)
                rel = relative_path(root, path) if path else None
                if rel is None:
                    return []
                selection.append((os.path.normpath(path), rel,
                                  file.is_directory()))
            binary = find_binary()
            if binary is None:
                return []
            specs = plan_file_menu(self._db, selection, binary,
                                   configured_provider())
        except Unavailable:
            return []
        return [self._make_item(spec) for spec in specs]

    @_guarded(list)
    def get_background_items(self, folder):
        path = local_path(folder)
        if path is None:
            return []
        try:
            root = self._db.sync_root()
            rel = relative_path(root, path)
            if rel is None:
                return []
            binary = find_binary()
            if binary is None:
                return []
            specs = plan_background_menu(
                self._db, os.path.normpath(path), rel, binary,
                configured_provider())
        except Unavailable:
            return []
        return [self._make_item(spec) for spec in specs]

    def _make_item(self, spec):
        # '_' marks a mnemonic in menu labels; double it to show it literally.
        item = Nautilus.MenuItem(
            name=spec.name, label=spec.label.replace('_', '__'),
            tip=spec.tip, sensitive=spec.sensitive)
        if spec.argv:
            item.connect('activate', self._on_activate, spec.argv)
        if spec.children:
            submenu = Nautilus.Menu()
            for child in spec.children:
                submenu.append_item(self._make_item(child))
            item.set_submenu(submenu)
        return item

    @staticmethod
    def _on_activate(_item, argv):
        launch(argv)
