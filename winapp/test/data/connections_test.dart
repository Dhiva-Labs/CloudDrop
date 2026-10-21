import 'dart:convert';
import 'dart:io';

import 'package:clouddrop_app/data/app_prefs.dart';
import 'package:clouddrop_app/data/connections.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

/// What `clouddrop connections --json` prints (src/main.rs › list_connections).
const listJson = '''
[
  {
    "config": "/home/me/.config/clouddrop/config.toml",
    "configured": true,
    "data_dir": "/home/me/.local/share/clouddrop",
    "drive_folder": "CloudDrop",
    "error": null,
    "folder": "/home/me/CloudDrop",
    "label": "CloudDrop",
    "name": "default",
    "provider": "gdrive",
    "provider_name": "Google Drive"
  },
  {
    "config": "/home/me/.config/clouddrop/connections/Work/config.toml",
    "configured": false,
    "data_dir": "/home/me/.local/share/clouddrop/connections/Work",
    "drive_folder": null,
    "error": "invalid config: TOML parse error",
    "folder": null,
    "label": "Work",
    "name": "Work",
    "provider": null,
    "provider_name": null
  }
]''';

void main() {
  group('connections --json', () {
    test('every field, the default connection first', () {
      final list = parseConnections(listJson);
      expect(list.map((c) => c.name), ['default', 'Work']);
      final d = list.first;
      expect(d.isDefault, isTrue);
      expect(d.label, 'CloudDrop');
      expect(d.folder, '/home/me/CloudDrop');
      expect((d.provider, d.providerName, d.driveFolder), ('gdrive', 'Google Drive', 'CloudDrop'));
      expect(d.config, '/home/me/.config/clouddrop/config.toml');
      expect(d.dataDir, '/home/me/.local/share/clouddrop');
      expect((d.configured, d.error), (true, null));
      final w = list.last;
      expect(w.isDefault, isFalse);
      expect((w.folder, w.provider, w.configured), (null, null, false));
      expect(w.error, 'invalid config: TOML parse error');
      expect(w.dataDir, '/home/me/.local/share/clouddrop/connections/Work');
    });

    test('before any setup the list is empty', () {
      expect(parseConnections('[]'), isEmpty);
    });

    test('anything but a list is refused', () {
      expect(() => parseConnections('{"name": "default"}'), throwsFormatException);
      expect(() => parseConnections('not json'), throwsFormatException);
    });

    test('a label defaults to the name', () {
      final c = Connection.fromJson(jsonDecode('{"name": "Photos"}') as Map<String, dynamic>);
      expect(c.label, 'Photos');
      expect(c == const Connection(name: 'Photos', label: 'Photos'), isTrue);
    });
  });

  group('connection names (connections::check_name)', () {
    test('accepted', () {
      for (final n in [
        'Work',
        'work 2',
        'a',
        'Photos-2026_v1.0',
        'Fotos Familie',
        'Документы',
        '照片',
      ]) {
        expect(connectionNameProblem(n), isNull, reason: n);
      }
      expect(connectionNameProblem('x' * 32), isNull);
    });

    test('refused', () {
      for (final n in [
        '',
        ' Work',
        '-work',
        '.hidden',
        'Work ',
        'Work.',
        'a/b',
        r'a\b',
        'what?',
        'x' * 33,
        'default',
        'Default',
        'con',
        'NUL',
        'com1',
        'LPT9',
        'aux.txt',
      ]) {
        expect(connectionNameProblem(n), isNotNull, reason: n);
      }
      // Not device names.
      expect(connectionNameProblem('COM10'), isNull);
      expect(connectionNameProblem('Console'), isNull);
    });
  });

  group('a new connection is named after its folder', () {
    test('the folder\'s name', () {
      expect(connectionNameFor('/home/me/Work', const []), 'Work');
      expect(connectionNameFor('/home/me/Work/', const []), 'Work');
      expect(
        connectionNameFor(r'C:\Users\Dhiva\Family Photos\', const [], context: p.windows),
        'Family Photos',
      );
    });

    test('unique ignoring case: "Work 2", "Work 3"', () {
      expect(connectionNameFor('/x/Work', ['default', 'work']), 'Work 2');
      expect(connectionNameFor('/x/Work', ['Work', 'WORK 2']), 'Work 3');
      // The default connection's label counts too: two "CloudDrop" rows would be confusing.
      expect(connectionNameFor('/mnt/d/CloudDrop', ['default', 'CloudDrop']), 'CloudDrop 2');
    });

    test('made valid exactly as the engine suggests (connections::suggest_name)', () {
      // The engine's own cases (src/connections.rs › suggested_names_are_valid).
      for (final (folder, want) in [
        ('/home/me/Work', 'Work'),
        ('/home/me/My Photos!', 'My Photos-'),
        ('/home/me/.hidden', 'hidden'),
        ('/home/me/con', 'con folder'),
        ('/home/me/default', 'default folder'),
        ('/', 'Folder'),
        ('/home/me/trailing. ', 'trailing'),
      ]) {
        final got = connectionNameFor(folder, const []);
        expect(got, want, reason: folder);
        expect(connectionNameProblem(got), isNull, reason: got);
      }
      expect(connectionNameFor('/x/My+Stuff (old)', const []), 'My-Stuff -old-');
      expect(connectionNameFor('/x/!!!', const []), 'Folder');
      expect(connectionNameFor('/x/Default', ['default folder']), 'Default folder 2');
      final long = connectionNameFor('/x/${'Quarterly reports ' * 3}', const []);
      expect(long, 'Quarterly reports Quarterly r');
      expect(long.length, lessThanOrEqualTo(32));
      final again = connectionNameFor('/x/${'Quarterly reports ' * 3}', [long]);
      expect(again, '$long 2');
      expect(connectionNameProblem(again), isNull);
    });
  });

  group('folders of their own', () {
    const conns = [
      Connection(name: 'default', label: 'CloudDrop', folder: '/home/me/CloudDrop'),
      Connection(name: 'Work', label: 'Work', folder: '/home/me/Work'),
      Connection(name: 'Broken', label: 'Broken'),
    ];

    test('the same folder, inside one, or holding one', () {
      expect(folderClash('/home/me/Photos', conns), isNull);
      expect(
        folderClash('/home/me/CloudDrop2', conns),
        isNull,
        reason: 'a name prefix is not inside',
      );
      var c = folderClash('/home/me/Work/', conns)!;
      expect((c.connection.name, c.clash), ('Work', FolderClash.same));
      c = folderClash('/home/me/CloudDrop/Photos', conns)!;
      expect((c.connection.name, c.clash), ('default', FolderClash.inside));
      c = folderClash('/home/me', conns)!;
      expect(c.clash, FolderClash.holds);
    });

    test('Windows folders ignore case', () {
      const win = [Connection(name: 'Work', label: 'Work', folder: r'C:\Users\Dhiva\Work')];
      final c = folderClash(r'c:\users\dhiva\WORK\Sub', win, context: p.windows)!;
      expect(c.clash, FolderClash.inside);
      expect(folderClash(r'C:\Users\Dhiva\Workshop', win, context: p.windows), isNull);
    });
  });

  group('app.json', () {
    late Directory dir;
    setUp(() => dir = Directory.systemTemp.createTempSync('clouddrop-prefs'));
    tearDown(() => dir.deleteSync(recursive: true));

    test('nothing remembered yet', () async {
      expect(await FileAppPrefs(p.join(dir.path, 'app.json')).connection(), isNull);
    });

    test('remembers the connection, keeping what else is in the file', () async {
      final path = p.join(dir.path, 'sub', 'app.json');
      final prefs = FileAppPrefs(path);
      await prefs.setConnection('Work');
      expect(jsonDecode(File(path).readAsStringSync()), {'connection': 'Work'});
      File(path).writeAsStringSync('{"connection": "Work", "gtk_width": 900}');
      await prefs.setConnection('default');
      expect(jsonDecode(File(path).readAsStringSync()), {
        'connection': 'default',
        'gtk_width': 900,
      });
      expect(await prefs.connection(), 'default');
      expect(File('$path.tmp').existsSync(), isFalse);
    });

    test('a broken or odd file reads as nothing', () async {
      final path = p.join(dir.path, 'app.json');
      File(path).writeAsStringSync('{not json');
      expect(await FileAppPrefs(path).connection(), isNull);
      File(path).writeAsStringSync('{"connection": 3}');
      expect(await FileAppPrefs(path).connection(), isNull);
      File(path).writeAsStringSync('["Work"]');
      expect(await FileAppPrefs(path).connection(), isNull);
      // ...and is replaced by a good one.
      await FileAppPrefs(path).setConnection('Work');
      expect(await FileAppPrefs(path).connection(), 'Work');
    });

    test('in memory', () async {
      final m = MemoryAppPrefs('Work');
      expect(await m.connection(), 'Work');
      await m.setConnection('default');
      expect(m.remembered, 'default');
    });
  });
}
