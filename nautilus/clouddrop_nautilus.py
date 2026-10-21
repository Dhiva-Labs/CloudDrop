"""CloudDrop integration for Nautilus (Files).

Shows the upload state of files in the CloudDrop folders as emblems and in a
column, and adds context-menu actions that run the ``clouddrop`` CLI.

Each folder is a connection (docs/PROVIDERS.md, "Several clouds at once"): one
sync folder uploading to one provider, with its own config.toml and data
directory. The default connection is config.toml and the data directory as
they always were; every other one is ``connections/<name>/`` beside them. A
file uses the connection whose folder holds it: that connection's database,
its provider's name in the menus, and ``--connection <name>`` in the commands
it runs. The column is named after the provider when every connection uses the
same one ("Google Drive" unless config.toml names another), else "CloudDrop".
State is read from the daemon's SQLite databases, opened read-only; this
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

DEFAULT_CONNECTION = 'default'
CONNECTIONS_DIR = 'connections'
MAX_CONNECTION_NAME = 32
NEUTRAL_COLUMN = 'CloudDrop'   # the column's title when the clouds differ

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
    """The data directory: the default connection's, holding the others'."""
    override = os.environ.get('CLOUDDROP_DATA_DIR')
    if override:
        return override
    base = os.environ.get('XDG_DATA_HOME') or os.path.join(
        os.path.expanduser('~'), '.local', 'share')
    return os.path.join(base, 'clouddrop')


def db_path():
    """The default connection's state database."""
    return os.path.join(data_dir(), 'state.db')


def config_path():
    """The default connection's config.toml; the others are beside it."""
    override = os.environ.get('CLOUDDROP_CONFIG')
    if override:
        return override
    base = os.environ.get('XDG_CONFIG_HOME') or os.path.join(
        os.path.expanduser('~'), '.config')
    return os.path.join(base, 'clouddrop', 'config.toml')


def connections_config_dir():
    """Where the named connections' config files are: beside config.toml."""
    return os.path.join(os.path.dirname(config_path()), CONNECTIONS_DIR)


def connection_config_path(name):
    if name == DEFAULT_CONNECTION:
        return config_path()
    return os.path.join(connections_config_dir(), name, 'config.toml')


def connection_db_path(name):
    if name == DEFAULT_CONNECTION:
        return db_path()
    return os.path.join(data_dir(), CONNECTIONS_DIR, name, 'state.db')


def valid_connection_name(name):
    """A name the CLI takes for a connection (check_name, src/connections.rs).

    1 to 32 letters, digits, spaces, '-', '_' or '.', starting with a letter
    or digit and not ending in a space or dot; not a Windows device name, and
    not "default", which is the default connection's.
    """
    if (not name or len(name) > MAX_CONNECTION_NAME or not name[0].isalnum()
            or name.endswith((' ', '.'))
            or not all(c.isalnum() or c in ' -_.' for c in name)):
        return False
    stem = name.split('.')[0].rstrip().upper()
    if stem in ('CON', 'PRN', 'AUX', 'NUL') or (
            len(stem) == 4 and stem[:3] in ('COM', 'LPT')
            and stem[3] in '0123456789'):
        return False
    return name.lower() != DEFAULT_CONNECTION


def connection_names():
    """The named connections (with a config.toml), sorted as the CLI does."""
    root = connections_config_dir()
    try:
        entries = os.listdir(root)
    except OSError:
        return []
    names = [name for name in entries if valid_connection_name(name)
             and os.path.isfile(os.path.join(root, name, 'config.toml'))]
    names.sort(key=lambda name: (name.lower(), name))
    return names


_config_cache = {}   # path -> (mtime_ns, parsed table or None)


def read_config(path):
    """A config.toml as a dict; None if missing or unreadable (mtime cache)."""
    try:
        mtime = os.stat(path).st_mtime_ns
    except OSError:
        _config_cache.pop(path, None)
        return None
    cached = _config_cache.get(path)
    if cached is not None and cached[0] == mtime:
        return cached[1]
    try:
        with open(path, 'rb') as f:
            data = tomllib.load(f)
    except (OSError, ValueError):
        data = None
    if len(_config_cache) >= 64:
        _config_cache.clear()
    _config_cache[path] = (mtime, data)
    return data


def keep_local_folders(path=None):
    """Folders marked "always keep on this computer" in a config.toml.

    The default connection's unless `path` names another config.toml.
    """
    data = read_config(path or config_path())
    keep = data.get('keep_local') if data else None
    if not isinstance(keep, list):
        return []
    return [str(k).strip('/') for k in keep]


def configured_folder(path):
    """The absolute sync_dir a config.toml names, normalised, else None."""
    data = read_config(path)
    value = data.get('sync_dir') if data else None
    if isinstance(value, str) and os.path.isabs(value):
        return os.path.normpath(value)
    return None


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


def configured_provider(path=None):
    """The provider a config.toml names; Google Drive if it names none.

    The default connection's config.toml unless `path` names another. Read
    through the mtime cache. An unknown id is shown as itself, without web
    links.
    """
    data = read_config(path or config_path())
    value = data.get('provider') if data else None
    if isinstance(value, str) and value:
        return PROVIDERS.get(value) or Provider(value, value, False)
    return GOOGLE_DRIVE


def column_title(providers):
    """The status column's (label, description) for the connections' clouds.

    Named after the provider when they all use the same one, else neutral.
    """
    providers = list(providers) or [GOOGLE_DRIVE]
    if len({p.id for p in providers}) == 1:
        name = providers[0].name
        return name, 'Upload status in %s' % name
    return (NEUTRAL_COLUMN,
            'Upload status in the cloud of each CloudDrop folder')


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
    """Read-only, non-blocking view of a CloudDrop state database.

    `path` is a function giving the file's path (looked up on every use); the
    default connection's database unless given.
    """

    def __init__(self, path=None):
        self._path = path or db_path
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
        path = self._path()
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


class Connection:
    """One sync folder uploading to one cloud: config.toml and a database.

    Paths are looked up on every use, from the environment as the CLI does.
    """

    def __init__(self, name):
        self.name = name
        self.db = StateDB(functools.partial(connection_db_path, name))
        self.root = None    # its folder as last learnt (database, else config)
        self.token = None   # (generation, data_version) at the last poll

    @property
    def config(self):
        return connection_config_path(self.name)

    def configured(self):
        return os.path.isfile(self.config)

    def provider(self):
        return configured_provider(self.config)

    def live_root(self):
        """Its folder as its database says now; None if that can't say."""
        try:
            return self.db.sync_root()
        except Unavailable:
            return None

    def learn_root(self, live):
        """Remember its folder: the database's, else config.toml's sync_dir.

        A database that can't say right now (busy, or not created yet) keeps
        the folder learnt before.
        """
        self.root = live or configured_folder(self.config) or self.root

    def forget(self):
        self.db.close()
        self.root = self.token = None


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


def command(binary, connection, *args):
    """A clouddrop command line for one connection, named explicitly."""
    return (binary, '--connection', connection, *args)


def plan_file_menu(db, selection, binary, provider=GOOGLE_DRIVE,
                   connection=DEFAULT_CONNECTION):
    """Menu for selected items; selection is [(abs_path, rel, is_dir)].

    All of them are in `connection`'s folder; `db` is its database.
    """
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
            command(binary, connection, '--notify', 'free', *free)))
    if (provider.web_links and len(selection) == 1
            and db.drive_id(selection[0][1])):
        specs.append(MenuSpec(
            'CloudDrop::open', 'Open in %s' % provider.name,
            'Open this item in %s' % provider.name,
            command(binary, connection, 'open', selection[0][0])))
    if retry:
        specs.append(MenuSpec(
            'CloudDrop::retry', 'Retry Upload',
            'Queue the failed uploads again',
            command(binary, connection, '--notify', 'retry', *retry)))
    if len(selection) == 1 and selection[0][1] and selection[0][2]:
        path, rel, _is_dir = selection[0]
        folders = keep_local_folders(connection_config_path(connection))
        if rel in folders:
            specs.append(MenuSpec(
                'CloudDrop::unkeep', 'Allow Automatic Free Up Space',
                'Let CloudDrop free this folder automatically again',
                command(binary, connection, '--notify', 'unkeep', path)))
        elif not any(rel.startswith(k + '/') for k in folders):
            specs.append(MenuSpec(
                'CloudDrop::keep', 'Always Keep on This Computer',
                'Automatic space saving will never free files in this folder',
                command(binary, connection, '--notify', 'keep', path)))
    return specs


def plan_background_menu(db, folder, rel, binary, provider=GOOGLE_DRIVE,
                         connection=DEFAULT_CONNECTION):
    """Menu for empty space in `folder` (absolute path, `rel` inside root)."""
    specs = []
    total, shown = db.cloud_children(rel, CLOUD_MENU_LIMIT)
    if total:
        children = [MenuSpec(
            'CloudDrop::restore_all', 'Download All',
            'Download every cloud-only item in this folder',
            command(binary, connection, '--notify', 'restore', folder))]
        for index, (name, _is_dir) in enumerate(shown):
            children.append(MenuSpec(
                'CloudDrop::restore_%d' % index, name,
                'Download %s from %s' % (name, provider.name),
                command(binary, connection, '--notify', 'restore',
                        os.path.join(folder, name))))
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
            command(binary, connection, 'open', folder)))
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
    connection: str = DEFAULT_CONNECTION


class CloudDropExtension(GObject.GObject, Nautilus.InfoProvider,
                         Nautilus.MenuProvider, Nautilus.ColumnProvider):

    def __init__(self):
        super().__init__()
        self._default = Connection(DEFAULT_CONNECTION)
        self._db = self._default.db              # the default connection's
        self._connections = []                    # default first, then by name
        self._default_config_seen = False
        self._seen = collections.OrderedDict()   # path -> Tracked, LRU order
        self._outside = collections.OrderedDict()  # path -> file in no folder
        self._queue = []                          # paths awaiting a re-check
        self._scan_connections()
        for conn in self._connections:
            conn.learn_root(conn.live_root())
        self._roots = self._root_key()
        GLib.timeout_add_seconds(POLL_INTERVAL_S, self._on_tick)

    # -- connections -------------------------------------------------------

    def _scan_connections(self):
        """Follow connections being added and removed.

        Costs a listing of the connections directory and a stat per
        connection, so it runs on the poll tick, never per file.
        """
        names = connection_names()
        if os.path.isfile(config_path()):
            self._default_config_seen = True
            names.insert(0, DEFAULT_CONNECTION)
        elif not self._default_config_seen:
            # A database alone is enough, as it always was; once its
            # config.toml has been seen, removing that removes the connection.
            names.insert(0, DEFAULT_CONNECTION)
        if names == [conn.name for conn in self._connections]:
            return
        old = {conn.name: conn for conn in self._connections}
        self._connections = [
            old.pop(name, None) or (
                self._default if name == DEFAULT_CONNECTION
                else Connection(name))
            for name in names]
        for conn in old.values():
            self._drop_connection(conn)

    def _drop_connection(self, conn):
        """A connection is gone: its items lose their emblems."""
        conn.forget()
        for path, tracked in list(self._seen.items()):
            if tracked.connection == conn.name:
                del self._seen[path]
                tracked.file.invalidate_extension_info()

    def _root_key(self):
        return tuple((conn.name, conn.root) for conn in self._connections)

    def _candidate(self, path):
        """The connection whose folder, as last learnt, holds `path`.

        Returns (connection, path relative to its folder), or (None, None).
        Touches no database, so items outside every folder cost nothing.
        """
        for conn in self._connections:
            if conn.root is not None:
                rel = relative_path(conn.root, path)
                if rel is not None:
                    return conn, rel
        return None, None

    def _locate(self, path):
        """Like _candidate, checked against that connection's database now.

        Raises Unavailable when the database can't say.
        """
        conn, _rel = self._candidate(path)
        if conn is None:
            return None, None
        rel = relative_path(conn.db.sync_root(), path)
        return (conn, rel) if rel is not None else (None, None)

    # -- ColumnProvider ----------------------------------------------------

    def get_columns(self):
        # Asked once at startup, so a provider switch, or a connection to
        # another provider, renames the column after a restart. Each file's
        # value comes from its own connection.
        providers = [conn.provider() for conn in self._connections
                     if conn.configured()] or [configured_provider()]
        label, description = column_title(providers)
        return [Nautilus.Column(
            name='CloudDrop::status', attribute=STATUS_ATTRIBUTE,
            label=label, description=description)]

    # -- InfoProvider ------------------------------------------------------

    @_guarded(lambda: None)
    def update_file_info(self, file):
        path = local_path(file)
        if path is None:
            return
        conn, rel = self._candidate(path)
        if conn is not None:
            try:
                rel = relative_path(conn.db.sync_root(), path)
            except Unavailable:
                pass   # go by the folder learnt last; the status is unknown
        if rel is None:
            self._remember_outside(path, file)
            return
        if not rel:
            return   # the folder itself
        self._outside.pop(path, None)
        is_dir = file.is_directory()
        try:
            status = compute_status(conn.db, rel, is_dir)
        except Unavailable:
            status = None
        self._remember(path, Tracked(file, is_dir, status, conn.name))
        if status is not None:
            file.add_string_attribute(STATUS_ATTRIBUTE, status.label)
            if status.emblem:
                file.add_emblem(status.emblem)

    def _remember(self, path, tracked):
        self._seen[path] = tracked
        self._seen.move_to_end(path)
        while len(self._seen) > MAX_TRACKED:
            self._seen.popitem(last=False)

    def _remember_outside(self, path, file):
        """Items in no folder are asked about again if a folder appears."""
        self._seen.pop(path, None)
        self._outside[path] = file
        self._outside.move_to_end(path)
        while len(self._outside) > MAX_TRACKED:
            self._outside.popitem(last=False)

    # -- live refresh ------------------------------------------------------

    @_guarded(lambda: True)
    def _on_tick(self):
        self.poll()
        return True   # keep the timer running

    def poll(self):
        """Follow connections coming and going, and re-check remembered files
        whose connection's database changed since last time.

        Work is bounded by POLL_BUDGET_S per call; leftovers carry over to the
        next call.
        """
        self._scan_connections()
        live, changed = {}, set()   # live: name -> (connection, its folder)
        for conn in self._connections:
            try:
                version = conn.db.data_version()
                live[conn.name] = (conn, conn.db.sync_root())
            except Unavailable:
                pass
            else:
                token = (conn.db.generation, version)
                if token != conn.token:
                    conn.token = token
                    changed.add(conn.name)
            conn.learn_root(live[conn.name][1] if conn.name in live else None)
        self._follow_roots()
        if changed:
            queued = set(self._queue)
            # Newest annotated is popped first.
            self._queue = [path for path, tracked in self._seen.items()
                           if tracked.connection in changed or path in queued]
        deadline = time.monotonic() + POLL_BUDGET_S
        waiting = []
        while self._queue:
            path = self._queue.pop()
            if not self._refresh(path, live):
                waiting.append(path)
            if time.monotonic() >= deadline:
                break
        self._queue.extend(reversed(waiting))

    def _follow_roots(self):
        """When a folder appears (a connection added, a database created),
        the items already shown in it are asked about again."""
        key = self._root_key()
        if key == self._roots:
            return
        self._roots = key
        for path, file in list(self._outside.items()):
            if file.is_gone():
                del self._outside[path]
                continue
            conn, rel = self._candidate(path)
            if conn is not None:
                del self._outside[path]
                if rel:
                    file.invalidate_extension_info()

    def _refresh(self, path, live):
        """Re-check a remembered file; False if its database can't say now."""
        tracked = self._seen.get(path)
        if tracked is None:
            return True
        if tracked.file.is_gone():
            del self._seen[path]
            return True
        if tracked.connection not in live:
            return False
        conn, root = live[tracked.connection]
        rel = relative_path(root, path)
        if not rel:
            # Its connection's folder moved: Files asks again where it is.
            del self._seen[path]
            tracked.file.invalidate_extension_info()
            return True
        try:
            status = compute_status(conn.db, rel, tracked.is_dir)
        except Unavailable:
            return True
        if status != tracked.status:
            self._seen[path] = tracked._replace(status=status)
            tracked.file.invalidate_extension_info()
        return True

    # -- MenuProvider ------------------------------------------------------

    @_guarded(list)
    def get_file_items(self, files):
        if not files:
            return []
        try:
            conn, selection = None, []
            for file in files:
                path = local_path(file)
                found, rel = self._locate(path) if path else (None, None)
                # Every item must be in the same connection's folder.
                if found is None or conn not in (None, found):
                    return []
                conn = found
                selection.append((os.path.normpath(path), rel,
                                  file.is_directory()))
            binary = find_binary()
            if binary is None:
                return []
            specs = plan_file_menu(conn.db, selection, binary,
                                   conn.provider(), conn.name)
        except Unavailable:
            return []
        return [self._make_item(spec) for spec in specs]

    @_guarded(list)
    def get_background_items(self, folder):
        path = local_path(folder)
        if path is None:
            return []
        try:
            conn, rel = self._locate(path)
            if conn is None:
                return []
            binary = find_binary()
            if binary is None:
                return []
            specs = plan_background_menu(
                conn.db, os.path.normpath(path), rel, binary,
                conn.provider(), conn.name)
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
