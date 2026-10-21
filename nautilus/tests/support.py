"""Test helpers: stub gi/Nautilus modules, fake file objects, temp databases.

Importing this module installs the stub ``gi`` modules and loads the
extension by path as ``ext``; it is shared by all test modules.
"""

import contextlib
import importlib.util
import os
import shutil
import sqlite3
import sys
import tempfile
import time
import types
import unittest
import unittest.mock

HERE = os.path.dirname(os.path.abspath(__file__))
EXTENSION_PATH = os.path.join(os.path.dirname(HERE), 'clouddrop_nautilus.py')

SCHEMA = """
CREATE TABLE entries (
  path        TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,
  state       TEXT NOT NULL,
  drive_id    TEXT,
  cloud_name  TEXT,
  cloud_parent TEXT,
  md5         TEXT,
  cloud_size  INTEGER,
  local_size  INTEGER,
  local_mtime_ns INTEGER,
  inode       INTEGER,
  upload_url  TEXT,
  upload_md5  TEXT,
  pin_prev    INTEGER NOT NULL DEFAULT 0,
  retry_at    INTEGER,
  error       TEXT,
  updated_at  INTEGER NOT NULL
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"""


# -- stub Nautilus API -------------------------------------------------------

class StubMenuItem:
    def __init__(self, name='', label='', tip='', icon='', sensitive=True):
        self.name = name
        self.label = label
        self.tip = tip
        self.sensitive = sensitive
        self.submenu = None
        self.handlers = []

    def connect(self, signal, callback, *args):
        self.handlers.append((signal, callback, args))

    def set_submenu(self, menu):
        self.submenu = menu

    def activate(self):
        for signal, callback, args in self.handlers:
            if signal == 'activate':
                callback(self, *args)


class StubMenu:
    def __init__(self):
        self.items = []

    def append_item(self, item):
        self.items.append(item)


class StubColumn:
    def __init__(self, name='', attribute='', label='', description=''):
        self.name = name
        self.attribute = attribute
        self.label = label
        self.description = description


class StubGLib:
    timeouts = []

    @classmethod
    def timeout_add_seconds(cls, interval, callback, *args):
        cls.timeouts.append((interval, callback))
        return len(cls.timeouts)


def _install_stubs():
    nautilus = types.SimpleNamespace(
        InfoProvider=type('InfoProvider', (), {}),
        MenuProvider=type('MenuProvider', (), {}),
        ColumnProvider=type('ColumnProvider', (), {}),
        MenuItem=StubMenuItem, Menu=StubMenu, Column=StubColumn)
    gobject = types.SimpleNamespace(GObject=type('GObject', (), {}))
    repository = types.ModuleType('gi.repository')
    repository.Nautilus = nautilus
    repository.GObject = gobject
    repository.GLib = StubGLib
    gi = types.ModuleType('gi')
    gi.require_version = lambda namespace, version: None
    gi.repository = repository
    sys.modules['gi'] = gi
    sys.modules['gi.repository'] = repository
    sys.modules['Nautilus'] = nautilus


def _load_extension():
    _install_stubs()
    spec = importlib.util.spec_from_file_location(
        'clouddrop_nautilus_under_test', EXTENSION_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ext = _load_extension()


# -- fake NautilusFileInfo ---------------------------------------------------

class FakeLocation:
    def __init__(self, path):
        self._path = path

    def get_path(self):
        return self._path


class FakeFile:
    def __init__(self, path, is_dir=False, scheme='file'):
        self.path = path
        self.is_dir = is_dir
        self.scheme = scheme
        self.gone = False
        self.emblems = []
        self.attributes = {}
        self.invalidated = 0

    def get_uri_scheme(self):
        return self.scheme

    def get_location(self):
        return FakeLocation(self.path) if self.path is not None else None

    def is_directory(self):
        return self.is_dir

    def is_gone(self):
        return self.gone

    def add_emblem(self, name):
        self.emblems.append(name)

    def add_string_attribute(self, name, value):
        self.attributes[name] = value

    def invalidate_extension_info(self):
        self.invalidated += 1

    @property
    def label(self):
        return self.attributes.get('clouddrop_status')


# -- temp database -----------------------------------------------------------

class Writer:
    """Plays the daemon: owns a writable connection to the state database."""

    def __init__(self, path, wal=True):
        self.path = path
        self.conn = sqlite3.connect(path, isolation_level=None)
        if wal:
            self.conn.execute('PRAGMA journal_mode=WAL')
        self.conn.executescript(SCHEMA)

    def set_meta(self, key, value):
        self.conn.execute('INSERT OR REPLACE INTO meta VALUES (?, ?)',
                          (key, value))

    def put(self, path, state='synced', kind='file', drive_id=None,
            error=None):
        self.conn.execute(
            'INSERT OR REPLACE INTO entries (path, kind, state, drive_id,'
            ' error, updated_at) VALUES (?, ?, ?, ?, ?, 0)',
            (path, kind, state, drive_id, error))

    def delete(self, path):
        self.conn.execute('DELETE FROM entries WHERE path = ?', (path,))

    def close(self):
        self.conn.close()


class ExtensionTestCase(unittest.TestCase):
    """Fresh temp dir, data dir, sync root and PATH with a clouddrop stub."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix='clouddrop-test-')
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.root = os.path.join(self.tmp, 'CloudDrop')
        self.data = os.path.join(self.tmp, 'data')
        os.makedirs(self.root)
        os.makedirs(self.data)
        self.db_file = os.path.join(self.data, 'state.db')
        self.binary = self.make_binary('bin')

        env = {'CLOUDDROP_DATA_DIR': self.data,
               'HOME': os.path.join(self.tmp, 'home'),
               'PATH': os.path.dirname(self.binary)}
        patcher = unittest.mock.patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)
        os.environ.pop('XDG_DATA_HOME', None)
        # Never read the developer's own config.toml.
        os.environ.pop('XDG_CONFIG_HOME', None)
        os.environ.pop('CLOUDDROP_CONFIG', None)
        self.writer = None
        self.extensions = []
        self.addCleanup(self._cleanup_extensions)

    def _cleanup_extensions(self):
        for extension in self.extensions:
            extension._db.close()
            for conn in extension._connections:
                conn.db.close()
        if self.writer is not None:
            self.writer.close()

    def make_binary(self, subdir, name='clouddrop'):
        directory = os.path.join(self.tmp, subdir)
        os.makedirs(directory, exist_ok=True)
        path = os.path.join(directory, name)
        with open(path, 'w') as handle:
            handle.write('#!/bin/sh\n')
        os.chmod(path, 0o755)
        return path

    def write_config(self, text):
        path = os.path.join(self.tmp, 'config.toml')
        with open(path, 'w') as f:
            f.write(text)
        # Distinct mtimes so the cache notices rewrites within one test.
        os.utime(path, ns=(time.time_ns(), time.time_ns()))
        return path

    def use_config(self, text):
        """Write config.toml and point CLOUDDROP_CONFIG at it for this test."""
        path = self.write_config(text)
        os.environ['CLOUDDROP_CONFIG'] = path   # setUp's patcher restores it
        return path

    def make_db(self, entries=(), root_id='ROOT', sync_root=None, wal=True):
        """Create state.db; entries are (path, state[, kind[, drive_id[, error]]])."""
        self.writer = Writer(self.db_file, wal=wal)
        self.writer.set_meta('schema_version', '1')
        self.writer.set_meta('sync_root', sync_root or self.root)
        if root_id:
            self.writer.set_meta('root_id', root_id)
        for entry in entries:
            self.put(*entry)
        return self.writer

    def put(self, path, state='synced', kind='file', drive_id=None,
            error=None):
        self.writer.put(path, state, kind, drive_id, error)

    def new_extension(self):
        extension = ext.CloudDropExtension()
        self.extensions.append(extension)
        return extension

    def abspath(self, rel):
        return os.path.join(self.root, rel) if rel else self.root

    def file(self, rel, is_dir=False):
        return FakeFile(self.abspath(rel), is_dir=is_dir)

    def annotate(self, extension, rel, is_dir=False):
        file = self.file(rel, is_dir)
        extension.update_file_info(file)
        return file


@contextlib.contextmanager
def no_binary_anywhere(test):
    """PATH and HOME contain no clouddrop binary."""
    empty = os.path.join(test.tmp, 'empty')
    os.makedirs(empty, exist_ok=True)
    with unittest.mock.patch.dict(
            os.environ, {'PATH': empty, 'HOME': os.path.join(test.tmp, 'h')}):
        yield
