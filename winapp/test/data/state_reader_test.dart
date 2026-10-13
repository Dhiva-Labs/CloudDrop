import 'dart:io';

import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

import '../support/test_db.dart';

List<String> paths(Iterable<Entry> es) => [for (final e in es) e.path];
List<String> itemPaths(Iterable<DirItem> es) => [for (final e in es) e.path];

void main() {
  group('queries on a real SQLite database (schema of src/db.rs)', () {
    late TestDb t;
    late StateDb db;

    setUp(() {
      t = TestDb.memory();
      db = StateDb(t.db);
    });
    tearDown(() => t.close());

    test('subtree and children respect path boundaries', () {
      for (final path in ['a', 'a/x', 'a/y/z', 'ab', 'a0', 'b']) {
        t.file(path, size: 1);
      }
      expect(paths(db.subtree('a')), ['a', 'a/x', 'a/y/z']);
      expect(paths(db.children('')), ['a', 'a0', 'ab', 'b']);
      expect(paths(db.children('a')), ['a/x']);
      expect(paths(db.children('a/y')), ['a/y/z']);
      expect(paths(db.subtree('')).length, 6);
    });

    test('children counts characters, not bytes, for non-ASCII folders', () {
      t.dir('Café ☕');
      t.file('Café ☕/résumé.pdf', size: 1);
      t.file('Café ☕/sub/deep.txt', size: 1);
      t.file('😀/x.txt', size: 1);
      expect(paths(db.children('Café ☕')), ['Café ☕/résumé.pdf']);
      expect(paths(db.children('😀')), ['😀/x.txt']);
    });

    test('listDir sorts folders first, then by name ignoring case, with stats', () {
      t.dir('Photos');
      t.file('Photos/a.jpg', size: 10);
      t.file('Photos/b.jpg', state: 'cloud_only', cloudSize: 20);
      t.file('Photos/c.jpg', state: 'error', size: 5, error: 'boom');
      t.file('zeta.txt', state: 'pending', size: 1);
      t.file('Alpha.txt', size: 3);
      t.dir('beta');
      final top = db.listDir('');
      expect(itemPaths(top), ['beta', 'Photos', 'Alpha.txt', 'zeta.txt']);
      final photos = top[1];
      final st = photos.stats!;
      expect(
        (st.files, st.bytes, st.local, st.cloudOnly, st.failed, st.waiting),
        (3, 35, 2, 1, 1, 0),
      );
      expect(st.cloudOnlyBytes, 20);
      expect(photos.bytes, 35);
      expect(top[3].bytes, 1);
      expect(top[3].stats, isNull);
      final all = db.folderStats('');
      expect((all.files, all.waiting), (5, 1));
    });

    test('file size: the Drive copy when cloud-only, else the local file', () {
      t.file('a', size: 7, cloudSize: 9);
      t.file('b', state: 'cloud_only', cloudSize: 20);
      expect(db.entry('a')!.size, 7);
      expect(db.entry('b')!.size, 20);
    });

    test('search matches names, not paths, anywhere; case-insensitive', () {
      t.dir('Photos');
      t.file('Photos/a.jpg', size: 1);
      t.file('Photos/b.JPG', size: 1);
      t.file('other/c.txt', size: 1);
      expect(itemPaths(db.search('jpg', 10)), ['Photos/a.jpg', 'Photos/b.JPG']);
      expect(db.search('photos/a', 10), isEmpty, reason: 'matches names, not paths');
      expect(itemPaths(db.search('PHOTOS', 10)), ['Photos']);
      expect(db.search('', 10), isEmpty);
      expect(db.search('jpg', 1).length, 1);
    });

    test('search treats % and _ literally', () {
      t.file('100%.txt', size: 1);
      t.file('1000.txt', size: 1);
      t.file('a_b.txt', size: 1);
      t.file('axb.txt', size: 1);
      expect(itemPaths(db.search('100%', 10)), ['100%.txt']);
      expect(itemPaths(db.search('a_b', 10)), ['a_b.txt']);
    });

    test('counts and cloud bytes', () {
      t.file('a', size: 10, cloudSize: 10);
      t.file('b', state: 'cloud_only', cloudSize: 20);
      t.file('c', state: 'pending', size: 5);
      t.file('d', state: 'error', size: 6, error: 'x');
      t.dir('dir');
      final c = {for (final r in db.counts()) '${r.kind}/${r.state}': r.count};
      expect(c['file/synced'], 1);
      expect(c['file/cloud_only'], 1);
      expect(c['file/pending'], 1);
      expect(c['file/error'], 1);
      expect(c['dir/synced'], 1);
      final b = db.cloudBytes();
      // Only files that have content in Drive (md5) count: synced + cloud-only.
      expect((b.all, b.cloudOnly), (30, 20));
    });

    test('activity is newest first; unknown kinds are skipped', () {
      t.activity('uploaded', 'a.txt', at: 1000, bytes: 3);
      t.activity('moved', 'b.txt', at: 2000, detail: 'a.txt');
      t.activity('something_new', 'c.txt', at: 3000);
      expect(db.lastActivityId(), 3);
      final acts = db.recentActivity(10);
      expect(acts.map((a) => a.kind), [ActivityKind.moved, ActivityKind.uploaded]);
      expect(acts[0].detail, 'a.txt');
      expect(acts[1].bytes, 3);
      // The limit applies before unknown kinds are dropped (like the engine's own query).
      expect(db.recentActivity(1), isEmpty);
      expect(db.recentActivity(2).length, 1);
    });

    test('lastActivityId is 0 with no history', () {
      expect(db.lastActivityId(), 0);
    });

    test('meta values', () {
      t.meta('root_id', 'R1');
      t.meta('paused', '1');
      expect(db.meta('root_id'), 'R1');
      expect(db.meta('paused'), '1');
      expect(db.meta('nope'), isNull);
    });

    test('browse: folder, filters, fallback to the nearest folder, search', () {
      t.dir('Photos');
      t.file('Photos/a.jpg', size: 10);
      t.file('Photos/b.jpg', state: 'cloud_only', cloudSize: 10);
      t.dir('Photos/Trip');
      t.file('Photos/Trip/c.jpg', state: 'error', size: 10, error: 'boom');
      t.file('top.txt', state: 'pending', size: 10);

      Listing q(Scope s, [Filter f = Filter.all]) => db.browse(Request(s, f));

      var l = q(const DirScope('Photos'));
      expect(itemPaths(l.items), ['Photos/Trip', 'Photos/a.jpg', 'Photos/b.jpg']);
      expect(l.stats!.files, 3);
      expect(l.dir, 'Photos');

      l = q(const DirScope(''), Filter.cloudOnly);
      expect(itemPaths(l.items), ['Photos/b.jpg']);
      l = q(const DirScope(''), Filter.failed);
      expect(itemPaths(l.items), ['Photos/Trip/c.jpg']);
      l = q(const DirScope(''), Filter.uploading);
      expect(itemPaths(l.items), ['top.txt']);
      l = q(const DirScope('Photos/Trip'), Filter.onComputer);
      expect(l.items.length, 1);

      // A folder that vanished falls back to the nearest one that still exists.
      l = q(const DirScope('Photos/Gone/Deeper'));
      expect(l.dir, 'Photos');
      // A path that is a file, not a folder, falls back too.
      expect(q(const DirScope('top.txt')).dir, '');

      expect(q(const SearchScope('JPG')).items.length, 3);
      expect(q(const SearchScope('jpg'), Filter.failed).items.length, 1);
      // A folder matches a filter when something beneath it does.
      expect(itemPaths(q(const SearchScope('trip'), Filter.failed).items), ['Photos/Trip']);
      expect(const Request(SearchScope('x'), Filter.all).isFlat, isTrue);
      expect(const Request(DirScope(''), Filter.failed).isFlat, isTrue);
      expect(const Request(DirScope(''), Filter.all).isFlat, isFalse);
    });

    test('flat listings are limited and say so', () {
      db.db.execute('BEGIN');
      for (var i = 0; i < flatLimit + 20; i++) {
        t.entry('f$i', state: 'error', md5: null, localSize: 1, error: 'x');
      }
      db.db.execute('COMMIT');
      final l = db.browse(const Request(DirScope(''), Filter.failed));
      expect(l.items.length, flatLimit);
      expect(l.truncated, isTrue);
    });

    test('unknown states are ignored rather than guessed', () {
      t.entry('weird', state: 'teleporting');
      t.file('fine', size: 1);
      expect(itemPaths(db.listDir('')), ['fine']);
    });
  });

  group('SqliteStateReader on a database file', () {
    late Directory tmp;
    late String path;

    setUp(() {
      tmp = Directory.systemTemp.createTempSync('clouddrop-db');
      path = p.join(tmp.path, 'state.db');
    });
    tearDown(() => tmp.deleteSync(recursive: true));

    test('a missing database reads as empty, not as an error', () async {
      final r = SqliteStateReader.file(path);
      expect(r.useIsolate, isTrue);
      expect((await r.browse(const Request(DirScope(''), Filter.all))).items, isEmpty);
      expect(await r.listDir(''), isEmpty);
      expect(await r.search('x'), isEmpty);
      expect(await r.counts(), isEmpty);
      expect(await r.lastActivityId(), 0);
      expect(await r.meta('root_id'), isNull);
      expect(await r.dataVersion(), isNull);
      expect(await r.entry('a'), isNull);
      r.close();
    });

    test('lists in a background isolate and sees the engine\'s later commits', () async {
      final w = TestDb.file(path);
      w.dir('Docs');
      w.file('Docs/a.txt', size: 4);
      final r = SqliteStateReader.file(path);
      var l = await r.browse(const Request(DirScope(''), Filter.all));
      expect(itemPaths(l.items), ['Docs']);
      expect(l.items.single.stats!.files, 1);

      final v1 = await r.dataVersion();
      expect(v1, isNotNull);
      expect(await r.dataVersion(), v1, reason: 'nothing changed');
      w.file('Docs/b.txt', size: 8);
      final v2 = await r.dataVersion();
      expect(v2, isNot(v1), reason: 'a commit by another connection changes data_version');

      l = await r.browse(const Request(DirScope('Docs'), Filter.all));
      expect(itemPaths(l.items), ['Docs/a.txt', 'Docs/b.txt']);
      expect(await r.search('b.t'), hasLength(1));
      expect((await r.folderStats('')).bytes, 12);

      // Cheap queries share one read-only connection.
      expect(await r.lastActivityId(), 0);
      w.activity('uploaded', 'Docs/a.txt', at: 5, bytes: 4);
      expect(await r.lastActivityId(), 1);
      expect((await r.recentActivity(5)).single.path, 'Docs/a.txt');
      r.close();
      w.close();
    });

    test('opens a WAL database whose writer already went away', () async {
      final w = TestDb.file(path);
      w.file('x.txt', size: 1);
      w.close(); // the last connection checkpoints and removes -wal/-shm
      final r = SqliteStateReader.file(path);
      expect(itemPaths((await r.listDir(''))), ['x.txt']);
      r.close();
    });

    test('never writes: the connection is read-only', () async {
      TestDb.file(path).close();
      final db = StateDb.openReadOnly(path);
      expect(() => db.db.execute("INSERT INTO meta VALUES ('x', 'y')"), throwsA(anything));
      db.close();
    });

    test('picks the database up once the engine creates it', () async {
      final r = SqliteStateReader.file(path);
      expect(await r.counts(), isEmpty);
      expect(await r.dataVersion(), isNull);
      final w = TestDb.file(path);
      w.file('new.txt', size: 1);
      w.file('new2.txt', size: 1);
      final counts = await r.counts();
      expect(counts.fold<int>(0, (s, c) => s + c.count), 2);
      expect(await r.dataVersion(), isNotNull);
      r.close();
      w.close();
    });
  });
}
