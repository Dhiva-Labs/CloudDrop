/// Read-only access to the engine's state database (`state.db`, SQLite, WAL).
///
/// The app never writes to it: everything that changes state goes through the
/// `clouddrop` CLI (see docs/WINDOWS.md). The queries here reproduce `list_dir`,
/// `search`, `folder_stats`, `recent_activity`, `last_activity_id`, `counts` and
/// `cloud_bytes` of `src/db.rs`.
library;

import 'dart:io';
import 'dart:isolate';

import 'package:sqlite3/sqlite3.dart';

import 'models.dart';

/// Rows shown at most when a filter lists files across a whole tree.
const int flatLimit = 5000;
const int searchLimit = 500;

/// Bytes the engine knows about: everything, and the part that is cloud-only.
class CloudBytes {
  const CloudBytes(this.all, this.cloudOnly);
  final int all;
  final int cloudOnly;
}

/// The reading side of the engine contract. UI code only sees this interface, so
/// tests can inject an in-memory database or a hand-written fake.
abstract class StateReader {
  /// The rows for one request: a folder (optionally filtered) or a search.
  Future<Listing> browse(Request req);

  Future<List<DirItem>> listDir(String dir);
  Future<List<DirItem>> search(String needle, {int limit = searchLimit});
  Future<Entry?> entry(String path);
  Future<FolderStats> folderStats(String dir);
  Future<List<StateCount>> counts();
  Future<CloudBytes> cloudBytes();
  Future<List<Activity>> recentActivity(int limit);
  Future<int> lastActivityId();
  Future<String?> meta(String key);

  /// `PRAGMA data_version` of a long-lived connection: changes whenever another
  /// connection (the engine, the CLI) commits. null = the database can't be read.
  Future<int?> dataVersion();

  void close();
}

const String _cols =
    'path, kind, state, drive_id, cloud_name, cloud_parent, md5, cloud_size, '
    'local_size, local_mtime_ns, inode, error, uploaded_at';

/// Synchronous queries on one open connection.
class StateDb {
  StateDb(this.db);

  final Database db;

  /// Opens `path` read-only. Throws [FileSystemException] if there is no database.
  factory StateDb.openReadOnly(String path) {
    if (!File(path).existsSync()) {
      throw FileSystemException('no state database', path);
    }
    final db = sqlite3.open(path, mode: OpenMode.readOnly);
    try {
      db.execute('PRAGMA busy_timeout = 3000');
    } catch (_) {
      // Not essential.
    }
    return StateDb(db);
  }

  void close() => db.close();

  Entry _entry(Row r) => Entry(
    path: r['path'] as String,
    kind: r['kind'] == 'dir' ? EntryKind.dir : EntryKind.file,
    state: EntryState.parse(r['state'] as String?) ?? EntryState.pending,
    driveId: r['drive_id'] as String?,
    cloudName: r['cloud_name'] as String?,
    cloudParent: r['cloud_parent'] as String?,
    md5: r['md5'] as String?,
    cloudSize: r['cloud_size'] as int?,
    localSize: r['local_size'] as int?,
    localMtimeNs: r['local_mtime_ns'] as int?,
    inode: r['inode'] as int?,
    error: r['error'] as String?,
    uploadedAt: r['uploaded_at'] as int?,
  );

  List<Entry> _entries(String sql, [List<Object?> params = const []]) {
    final out = <Entry>[];
    for (final r in db.select(sql, params)) {
      // A state this app doesn't know yet: skip the row rather than guess.
      if (EntryState.parse(r['state'] as String?) == null) continue;
      out.add(_entry(r));
    }
    return out;
  }

  String? meta(String key) {
    final rows = db.select('SELECT value FROM meta WHERE key = ?1', [key]);
    return rows.isEmpty ? null : rows.first['value'] as String?;
  }

  Entry? entry(String path) {
    final rows = _entries('SELECT $_cols FROM entries WHERE path = ?1', [path]);
    return rows.isEmpty ? null : rows.first;
  }

  /// `prefix` and all entries beneath it, parents before children. "" = everything.
  List<Entry> subtree(String prefix) {
    if (prefix.isEmpty) {
      return _entries('SELECT $_cols FROM entries ORDER BY path');
    }
    return _entries(
      'SELECT $_cols FROM entries '
      'WHERE (path = ?1 OR (path >= ?2 AND path < ?3)) ORDER BY path',
      [prefix, '$prefix/', '${prefix}0'],
    );
  }

  /// Direct children of `dir` ("" = top level).
  List<Entry> children(String dir) {
    if (dir.isEmpty) {
      return _entries("SELECT $_cols FROM entries WHERE instr(path, '/') = 0 ORDER BY path");
    }
    // SQLite's substr counts characters, so count runes, not UTF-16 units.
    final skip = dir.runes.length + 2;
    return _entries(
      'SELECT $_cols FROM entries WHERE path >= ?1 AND path < ?2 '
      "AND instr(substr(path, ?3), '/') = 0 ORDER BY path",
      ['$dir/', '${dir}0', skip],
    );
  }

  List<StateCount> counts() {
    final out = <StateCount>[];
    for (final r in db.select(
      'SELECT kind, state, count(*) AS n FROM entries GROUP BY kind, state',
    )) {
      out.add(StateCount(r['kind'] as String, r['state'] as String, r['n'] as int));
    }
    return out;
  }

  CloudBytes cloudBytes() {
    final r = db
        .select(
          'SELECT coalesce(sum(cloud_size), 0) AS a, '
          "coalesce(sum(CASE WHEN state = 'cloud_only' THEN cloud_size END), 0) AS c "
          "FROM entries WHERE kind = 'file' AND md5 IS NOT NULL",
        )
        .first;
    return CloudBytes(r['a'] as int, r['c'] as int);
  }

  /// Totals for the files beneath `dir` ("" = everything).
  FolderStats folderStats(String dir) {
    final lo = dir.isEmpty ? '' : '$dir/';
    final hi = dir.isEmpty ? '\u{10FFFF}' : '${dir}0';
    final r = db.select(
      'SELECT count(*) AS files, '
      "coalesce(sum(CASE WHEN state = 'cloud_only' THEN coalesce(cloud_size, 0) "
      'ELSE coalesce(local_size, cloud_size, 0) END), 0) AS bytes, '
      "count(*) FILTER (WHERE state <> 'cloud_only') AS local, "
      "count(*) FILTER (WHERE state = 'cloud_only') AS cloud_only, "
      "count(*) FILTER (WHERE state IN ('pending', 'uploading')) AS waiting, "
      "count(*) FILTER (WHERE state = 'error') AS failed, "
      "coalesce(sum(cloud_size) FILTER (WHERE state = 'cloud_only'), 0) AS cloud_only_bytes "
      "FROM entries WHERE kind = 'file' AND path >= ?1 AND path < ?2",
      [lo, hi],
    ).first;
    return FolderStats(
      files: r['files'] as int,
      bytes: r['bytes'] as int,
      local: r['local'] as int,
      cloudOnly: r['cloud_only'] as int,
      waiting: r['waiting'] as int,
      failed: r['failed'] as int,
      cloudOnlyBytes: r['cloud_only_bytes'] as int,
    );
  }

  DirItem dirItem(Entry e) {
    if (e.isDir) {
      final s = folderStats(e.path);
      return DirItem(entry: e, bytes: s.bytes, stats: s);
    }
    return DirItem.file(e);
  }

  /// Direct children of `dir` for the file browser, folders first then by name.
  List<DirItem> listDir(String dir) {
    final kids = children(dir);
    kids.sort((a, b) {
      final d = (a.isDir ? 0 : 1).compareTo(b.isDir ? 0 : 1);
      if (d != 0) return d;
      return a.name.toLowerCase().compareTo(b.name.toLowerCase());
    });
    return [for (final e in kids) dirItem(e)];
  }

  /// Files and folders whose name contains `needle` (case-insensitive), anywhere.
  List<DirItem> search(String needle, int limit) {
    final n = needle.toLowerCase();
    if (n.isEmpty) return const [];
    final pattern = '%${n.replaceAll(r'\', r'\\').replaceAll('%', r'\%').replaceAll('_', r'\_')}%';
    final hits = _entries(
      'SELECT $_cols FROM entries WHERE lower(path) LIKE ?1 ESCAPE \'\\\' '
      "ORDER BY kind = 'dir' DESC, path LIMIT ?2",
      [pattern, limit * 4],
    );
    final out = <DirItem>[];
    for (final e in hits) {
      if (!e.name.toLowerCase().contains(n)) continue;
      out.add(dirItem(e));
      if (out.length >= limit) break;
    }
    return out;
  }

  List<Activity> recentActivity(int limit) {
    final out = <Activity>[];
    for (final r in db.select(
      'SELECT id, at, kind, path, bytes, detail FROM activity ORDER BY id DESC LIMIT ?1',
      [limit],
    )) {
      final kind = ActivityKind.parse(r['kind'] as String?);
      if (kind == null) continue;
      out.add(
        Activity(
          id: r['id'] as int,
          atMs: r['at'] as int,
          kind: kind,
          path: r['path'] as String,
          bytes: r['bytes'] as int?,
          detail: r['detail'] as String?,
        ),
      );
    }
    return out;
  }

  int lastActivityId() =>
      db.select('SELECT coalesce(max(id), 0) AS m FROM activity').first['m'] as int;

  /// `PRAGMA data_version` moves when *another* connection commits (the engine, the CLI);
  /// `total_changes()` moves when this very connection writes (only tests do that, the app's
  /// connection is read-only). Together they change whenever the data did.
  int? dataVersion() {
    final rows = db.select('PRAGMA data_version');
    if (rows.isEmpty) return null;
    final v = rows.first.values.first as int?;
    if (v == null) return null;
    final own = db.select('SELECT total_changes() AS n').first['n'] as int;
    return v * 1000000007 + own;
  }

  /// Nearest ancestor of `dir` (or `dir` itself) that is still a tracked folder; "" = the top.
  String resolveDir(String dir) {
    var d = dir;
    while (d.isNotEmpty) {
      final e = entry(d);
      if (e != null && e.isDir) return d;
      d = parentOf(d);
    }
    return '';
  }

  /// The rows for a request (the file browser's whole query in one go).
  Listing browse(Request req) {
    final scope = req.scope;
    switch (scope) {
      case DirScope():
        final dir = resolveDir(scope.dir);
        final stats = folderStats(dir);
        if (req.filter == Filter.all) {
          return Listing(items: listDir(dir), stats: stats, dir: dir);
        }
        final items = <DirItem>[];
        var truncated = false;
        for (final e in subtree(dir)) {
          if (e.isDir || !matchesState(req.filter, e.state)) continue;
          if (items.length >= flatLimit) {
            truncated = true;
            break;
          }
          items.add(DirItem.file(e));
        }
        return Listing(items: items, truncated: truncated, stats: stats, dir: dir);
      case SearchScope():
        final hits = search(
          scope.needle,
          searchLimit + 1,
        ).where((i) => matchesItem(req.filter, i)).toList();
        final truncated = hits.length > searchLimit;
        return Listing(
          items: truncated ? hits.sublist(0, searchLimit) : hits,
          truncated: truncated,
        );
    }
  }
}

bool matchesState(Filter f, EntryState s) => switch (f) {
  Filter.all => true,
  Filter.onComputer => s != EntryState.cloudOnly,
  Filter.cloudOnly => s == EntryState.cloudOnly,
  Filter.uploading => s == EntryState.pending || s == EntryState.uploading,
  Filter.failed => s == EntryState.error,
};

/// Folders match when something beneath them does.
bool matchesItem(Filter f, DirItem item) {
  final s = item.stats;
  if (item.isDir && s != null) {
    return switch (f) {
      Filter.all => true,
      Filter.onComputer => s.local > 0,
      Filter.cloudOnly => s.cloudOnly > 0,
      Filter.uploading => s.waiting > 0,
      Filter.failed => s.failed > 0,
    };
  }
  return matchesState(f, item.entry.state);
}

// ---------- the SQLite-backed reader ----------

Listing _browseIn(String path, Request req) {
  final db = StateDb.openReadOnly(path);
  try {
    return db.browse(req);
  } finally {
    db.close();
  }
}

List<DirItem> _listDirIn(String path, String dir) {
  final db = StateDb.openReadOnly(path);
  try {
    return db.listDir(dir);
  } finally {
    db.close();
  }
}

List<DirItem> _searchIn(String path, String needle, int limit) {
  final db = StateDb.openReadOnly(path);
  try {
    return db.search(needle, limit);
  } finally {
    db.close();
  }
}

/// Reads `state.db`. Listings run in a short-lived isolate on their own connection, so a
/// big folder never stalls the UI; the small once-a-second queries share one connection.
class SqliteStateReader implements StateReader {
  /// Reads the database file at [path] (it may not exist yet).
  SqliteStateReader.file(String this.path, {this.useIsolate = true}) : _fixed = null;

  /// Reads one already open database (tests: in-memory databases).
  SqliteStateReader.shared(StateDb db) : path = null, useIsolate = false, _fixed = db;

  final String? path;
  final bool useIsolate;
  final StateDb? _fixed;
  StateDb? _main;
  bool _closed = false;

  StateDb? _conn() {
    if (_fixed != null) return _fixed;
    if (_closed) return null;
    if (_main != null) return _main;
    try {
      return _main = StateDb.openReadOnly(path!);
    } on FileSystemException {
      return null;
    } on SqliteException {
      return null;
    }
  }

  void _drop() {
    if (_fixed != null) return;
    try {
      _main?.close();
    } catch (_) {}
    _main = null;
  }

  /// Runs a small query on the shared connection; [fallback] if the database is missing
  /// or busy. One reconnect-and-retry covers a database that was replaced.
  T _cheap<T>(T Function(StateDb db) f, T fallback) {
    for (var attempt = 0; attempt < 2; attempt++) {
      final db = _conn();
      if (db == null) return fallback;
      try {
        return f(db);
      } on SqliteException {
        _drop();
      }
    }
    return fallback;
  }

  Future<T> _heavy<T>(T Function(StateDb db) inProcess, T Function() inIsolate) async {
    if (_fixed != null || !useIsolate) {
      final db = _conn();
      if (db == null) throw const FileSystemException('no state database');
      return inProcess(db);
    }
    return Isolate.run(inIsolate);
  }

  @override
  Future<Listing> browse(Request req) async {
    final p = path;
    if (p != null && _fixed == null && !File(p).existsSync()) {
      return const Listing();
    }
    return _heavy((db) => db.browse(req), () => _browseIn(p!, req));
  }

  @override
  Future<List<DirItem>> listDir(String dir) async {
    final p = path;
    if (p != null && _fixed == null && !File(p).existsSync()) return const [];
    return _heavy((db) => db.listDir(dir), () => _listDirIn(p!, dir));
  }

  @override
  Future<List<DirItem>> search(String needle, {int limit = searchLimit}) async {
    final p = path;
    if (p != null && _fixed == null && !File(p).existsSync()) return const [];
    return _heavy((db) => db.search(needle, limit), () => _searchIn(p!, needle, limit));
  }

  @override
  Future<Entry?> entry(String path) async => _cheap((db) => db.entry(path), null);

  @override
  Future<FolderStats> folderStats(String dir) async =>
      _cheap((db) => db.folderStats(dir), const FolderStats());

  @override
  Future<List<StateCount>> counts() async => _cheap((db) => db.counts(), const <StateCount>[]);

  @override
  Future<CloudBytes> cloudBytes() async => _cheap((db) => db.cloudBytes(), const CloudBytes(0, 0));

  @override
  Future<List<Activity>> recentActivity(int limit) async =>
      _cheap((db) => db.recentActivity(limit), const <Activity>[]);

  @override
  Future<int> lastActivityId() async => _cheap((db) => db.lastActivityId(), 0);

  @override
  Future<String?> meta(String key) async => _cheap((db) => db.meta(key), null);

  @override
  Future<int?> dataVersion() async => _cheap((db) => db.dataVersion(), null);

  @override
  void close() {
    _closed = true;
    _drop();
  }
}
