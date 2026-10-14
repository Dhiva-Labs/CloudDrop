import 'dart:io';

import 'package:clouddrop_app/data/fs_ops.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

void main() {
  test('numbered names match util::numbered_name', () {
    expect(numberedName('report.pdf', 2), 'report (2).pdf');
    expect(numberedName('.bashrc', 3), '.bashrc (3)');
    expect(numberedName('Makefile', 2), 'Makefile (2)');
    expect(numberedName('a.tar.gz', 2), 'a.tar (2).gz');
  });

  test('unique names skip taken ones', () {
    expect(uniqueName('a.txt', (_) => false), 'a.txt');
    expect(uniqueName('a.txt', (c) => c == 'a.txt' || c == 'a (2).txt'), 'a (3).txt');
    expect(uniqueName('Photos', (c) => c == 'Photos'), 'Photos (2)');
  });

  test('relative to the sync folder', () {
    const root = '/home/me/CloudDrop';
    expect(relInSync(root, '/home/me/CloudDrop/Photos/2026', context: p.posix), 'Photos/2026');
    expect(relInSync(root, '/home/me/CloudDrop', context: p.posix), isNull);
    expect(relInSync(root, '/home/me/Other', context: p.posix), isNull);
    expect(relInSync(root, '/home/me/CloudDropX/a', context: p.posix), isNull);
    expect(relInSync(root, '/home/me/CloudDrop/Photos/../Work', context: p.posix), 'Work');
    expect(
      relInSync(r'C:\Users\me\CloudDrop', r'C:\Users\me\CloudDrop\Photos\2026', context: p.windows),
      'Photos/2026',
    );
    expect(relInSync(r'C:\Users\me\CloudDrop', r'D:\Other', context: p.windows), isNull);
  });

  test('keep list editing', () {
    const list = ['Work/Old', 'Photos'];
    expect(addKeep(list, 'Photos/2026'), isNull);
    expect(addKeep(list, 'Photos'), isNull);
    expect(addKeep(list, ''), isNull);
    expect(addKeep(list, 'Work'), ['Photos', 'Work']);
    expect(covered(list, 'Workshop/x'), isFalse);
    expect(covered(list, 'Work/Old/x'), isTrue);
    expect(covered(const ['Photos/'], 'Photos/a'), isTrue);
  });

  group('copying into the CloudDrop folder', () {
    late Directory tmp;
    late String src;
    late String dest;

    setUp(() {
      tmp = Directory.systemTemp.createTempSync('clouddrop-copy');
      src = p.join(tmp.path, 'src');
      dest = p.join(tmp.path, 'dest');
      Directory(p.join(src, 'album', 'inner')).createSync(recursive: true);
      File(p.join(src, 'a.txt')).writeAsStringSync('new');
      File(p.join(src, 'album', 'b.txt')).writeAsStringSync('b');
      File(p.join(src, 'album', 'inner', 'c.txt')).writeAsStringSync('c');
      Directory(dest).createSync();
      File(p.join(dest, 'a.txt')).writeAsStringSync('old');
    });
    tearDown(() => tmp.deleteSync(recursive: true));

    test('copies without overwriting; names get (2), (3), ...', () {
      final r = copyInto(dest, [p.join(src, 'a.txt'), p.join(src, 'album')]);
      expect((r.items, r.files), (2, 3));
      expect(r.problems, isEmpty);
      expect(File(p.join(dest, 'a.txt')).readAsStringSync(), 'old');
      expect(File(p.join(dest, 'a (2).txt')).readAsStringSync(), 'new');
      expect(File(p.join(dest, 'album', 'inner', 'c.txt')).readAsStringSync(), 'c');

      // A second drop of the same folder gets its own copy, and no temp files are left behind.
      final r2 = copyInto(dest, [p.join(src, 'album')]);
      expect(r2.items, 1);
      expect(File(p.join(dest, 'album (2)', 'b.txt')).existsSync(), isTrue);
      final leftovers = Directory(
        dest,
      ).listSync(recursive: true).where((e) => e.path.endsWith('.part')).toList();
      expect(leftovers, isEmpty);
      expect(r2.summary('CloudDrop'), 'Copied 1 item to CloudDrop. Uploading will start shortly.');
      expect(r.summary('Docs'), 'Copied 2 items to Docs. Uploading will start shortly.');
    });

    test('keeps the file\'s modification time', () {
      final f = File(p.join(src, 'a.txt'));
      final when = DateTime.utc(2020, 5, 17, 10, 30);
      f.setLastModifiedSync(when);
      copyInto(dest, [f.path]);
      expect(File(p.join(dest, 'a (2).txt')).lastModifiedSync().toUtc(), when);
    });

    test('names that exist only in Drive are avoided (case-insensitively on Windows)', () {
      File(p.join(src, 'report.pdf')).writeAsStringSync('x');
      final r = copyInto(dest, [p.join(src, 'report.pdf')], reservedNames: {'report.pdf'});
      expect(r.items, 1);
      expect(File(p.join(dest, 'report (2).pdf')).existsSync(), isTrue);
      expect(File(p.join(dest, 'report.pdf')).existsSync(), isFalse);

      File(p.join(src, 'Notes.TXT')).writeAsStringSync('n');
      final r2 = copyInto(
        dest,
        [p.join(src, 'Notes.TXT')],
        reservedNames: {'notes.txt'},
        caseInsensitive: true,
      );
      expect(r2.items, 1);
      expect(File(p.join(dest, 'Notes (2).TXT')).existsSync(), isTrue);
    });

    test('two sources with the same name do not clobber each other', () {
      final other = Directory(p.join(tmp.path, 'other'))..createSync();
      File(p.join(other.path, 'a.txt')).writeAsStringSync('other');
      final r = copyInto(dest, [p.join(src, 'a.txt'), p.join(other.path, 'a.txt')]);
      expect(r.items, 2);
      expect(File(p.join(dest, 'a (2).txt')).readAsStringSync(), 'new');
      expect(File(p.join(dest, 'a (3).txt')).readAsStringSync(), 'other');
    });

    test('creates the destination folder if it vanished', () {
      final deep = p.join(dest, 'new', 'deeper');
      final r = copyInto(deep, [p.join(src, 'a.txt')]);
      expect(r.items, 1);
      expect(File(p.join(deep, 'a.txt')).readAsStringSync(), 'new');
    });

    test('refuses to copy a folder into itself', () {
      final sub = p.join(src, 'album', 'inner');
      final r = copyInto(sub, [p.join(src, 'album')]);
      expect(r.items, 0);
      expect(r.problems, hasLength(1));
      expect(r.summary('x'), startsWith('Nothing was copied.'));
    });

    test('skips symlinks and reports unreadable sources', () {
      Link(p.join(src, 'link')).createSync(p.join(src, 'a.txt'));
      final r = copyInto(dest, [p.join(src, 'link'), p.join(src, 'missing.txt')]);
      expect(r.items, 0);
      expect(Directory(dest).listSync().map((e) => p.basename(e.path)), ['a.txt']);
    });

    test('copyIntoAsync runs on another isolate', () async {
      final r = await copyIntoAsync(dest, [p.join(src, 'album')]);
      expect(r.items, 1);
      expect(File(p.join(dest, 'album', 'b.txt')).existsSync(), isTrue);
    });
  });
}
