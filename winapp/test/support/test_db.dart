/// Real SQLite databases with the exact schema of `src/db.rs`, for tests.
library;

import 'package:clouddrop_app/data/state_reader.dart';
import 'package:sqlite3/sqlite3.dart';

/// Copied from `SCHEMA` in `src/db.rs` (schema_version 3).
const String engineSchema = '''
CREATE TABLE IF NOT EXISTS entries (
  path TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  state TEXT NOT NULL,
  drive_id TEXT,
  cloud_name TEXT,
  cloud_parent TEXT,
  md5 TEXT,
  cloud_size INTEGER,
  local_size INTEGER,
  local_mtime_ns INTEGER,
  inode INTEGER,
  upload_url TEXT,
  upload_md5 TEXT,
  pin_prev INTEGER NOT NULL DEFAULT 0,
  move_pending INTEGER NOT NULL DEFAULT 0,
  attempts INTEGER NOT NULL DEFAULT 0,
  retry_at INTEGER,
  error TEXT,
  updated_at INTEGER NOT NULL,
  uploaded_at INTEGER,
  hash_kind TEXT
);
CREATE INDEX IF NOT EXISTS entries_state ON entries(state);
CREATE INDEX IF NOT EXISTS entries_inode ON entries(inode);
CREATE INDEX IF NOT EXISTS entries_drive_id ON entries(drive_id);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  path TEXT NOT NULL,
  bytes INTEGER,
  detail TEXT
);
''';

/// A writable handle on a test database (the app itself only ever reads).
class TestDb {
  TestDb._(this.db, this.path);

  /// An in-memory database.
  factory TestDb.memory() {
    final db = sqlite3.openInMemory();
    db.execute(engineSchema);
    db.execute("INSERT INTO meta VALUES ('schema_version', '3')");
    return TestDb._(db, null);
  }

  /// A WAL-mode database file, like the engine's.
  factory TestDb.file(String path) {
    final db = sqlite3.open(path);
    db.execute('PRAGMA journal_mode=WAL');
    db.execute(engineSchema);
    db.execute("INSERT OR REPLACE INTO meta VALUES ('schema_version', '3')");
    return TestDb._(db, path);
  }

  final Database db;
  final String? path;

  void close() => db.close();

  /// A reader over this very database (in-memory ones included).
  SqliteStateReader reader() => SqliteStateReader.shared(StateDb(db));

  void file(
    String path, {
    String state = 'synced',
    int? size,
    int? cloudSize,
    int? mtimeMs,
    String? driveId,
    String? md5,
    String? error,
    int? uploadedAt,
  }) {
    entry(
      path,
      kind: 'file',
      state: state,
      localSize: state == 'cloud_only' ? null : size,
      cloudSize: cloudSize ?? size,
      mtimeMs: state == 'cloud_only' ? null : mtimeMs,
      driveId: driveId ?? (state == 'pending' ? null : 'id-$path'),
      md5: md5 ?? (state == 'synced' || state == 'cloud_only' ? 'md5-$path' : null),
      error: error,
      uploadedAt: uploadedAt,
    );
  }

  void dir(String path, {String state = 'synced', String? driveId}) {
    entry(path, kind: 'dir', state: state, driveId: driveId ?? 'id-$path');
  }

  void entry(
    String path, {
    String kind = 'file',
    String state = 'synced',
    String? driveId,
    String? md5,
    int? cloudSize,
    int? localSize,
    int? mtimeMs,
    String? error,
    int? uploadedAt,
  }) {
    db.execute(
      'INSERT OR REPLACE INTO entries (path, kind, state, drive_id, md5, cloud_size, '
      'local_size, local_mtime_ns, error, updated_at, uploaded_at) '
      'VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)',
      [
        path,
        kind,
        state,
        driveId,
        md5,
        cloudSize,
        localSize,
        mtimeMs == null ? null : mtimeMs * 1000000,
        error,
        1,
        uploadedAt,
      ],
    );
  }

  void activity(String kind, String path, {required int at, int? bytes, String? detail}) {
    db.execute('INSERT INTO activity (at, kind, path, bytes, detail) VALUES (?1, ?2, ?3, ?4, ?5)', [
      at,
      kind,
      path,
      bytes,
      detail,
    ]);
  }

  void meta(String key, String value) {
    db.execute(
      'INSERT INTO meta(key, value) VALUES (?1, ?2) '
      'ON CONFLICT(key) DO UPDATE SET value = excluded.value',
      [key, value],
    );
  }
}
