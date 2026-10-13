@Tags(['integration'])
library;

/// The app's data layer and controllers against the REAL engine: the Linux `clouddrop`
/// binary and daemon, talking to the fake Google Drive (`examples/mock_drive`), all inside
/// a throw-away sandbox. Nothing here touches the real CloudDrop installation:
///
///  - CLOUDDROP_CONFIG / CLOUDDROP_DATA_DIR / HOME point into a temp directory,
///  - the processes get an empty environment (no session bus, no DISPLAY) and a PATH
///    with no programs in it, so `systemctl` and `xdg-open` cannot even be found,
///  - only the processes started here are ever killed (by PID).
///
/// Build first:   cargo build -p clouddrop --example mock_drive && cargo build -p clouddrop
/// Run:           flutter test --tags integration test/integration
/// Override the binaries with CLOUDDROP_TEST_BIN and CLOUDDROP_TEST_MOCK.

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/paths.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:clouddrop_app/data/status_source.dart';
import 'package:clouddrop_app/logic/files_model.dart';
import 'package:clouddrop_app/logic/snapshot.dart';
import 'package:clouddrop_app/state/activity_controller.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/state/files_controller.dart';
import 'package:clouddrop_app/ui/app.dart';
import 'package:fluent_ui/fluent_ui.dart' show ThemeMode;
import 'package:flutter/widgets.dart' show SizedBox;
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

import '../support/fakes.dart';
import '../support/harness.dart';

const String mockAddr = '127.0.0.1:8299';

String _find(String envName, String relative) {
  final fromEnv = Platform.environment[envName];
  if (fromEnv != null && fromEnv.isNotEmpty) return fromEnv;
  return p.normalize(p.join(Directory.current.path, '..', relative));
}

final String cliBin = _find('CLOUDDROP_TEST_BIN', 'target/debug/clouddrop');
final String mockBin = _find('CLOUDDROP_TEST_MOCK', 'target/debug/examples/mock_drive');
final bool haveBinaries = File(cliBin).existsSync() && File(mockBin).existsSync();

/// One isolated world: its own HOME, config, data directory, sync folder and environment.
class Sandbox {
  Sandbox(String name) : root = Directory.systemTemp.createTempSync('clouddrop-it-$name-') {
    emptyBin = Directory(p.join(root.path, 'emptybin'))..createSync();
    home = p.join(root.path, 'home');
    configPath = p.join(home, '.config', 'clouddrop', 'config.toml');
    dataDir = p.join(home, '.local', 'share', 'clouddrop');
    syncDir = p.join(home, 'CloudDrop');
    Directory(p.dirname(configPath)).createSync(recursive: true);
    Directory(dataDir).createSync(recursive: true);
  }

  final Directory root;
  late final Directory emptyBin;
  late final String home;
  late final String configPath;
  late final String dataDir;
  late final String syncDir;

  /// The only environment the engine and the app's CLI calls get.
  Map<String, String> get env => {
    'HOME': home,
    'PATH': emptyBin.path,
    'CLOUDDROP_CONFIG': configPath,
    'CLOUDDROP_DATA_DIR': dataDir,
    'CLOUDDROP_LOG': 'clouddrop=info',
    'LANG': 'C.UTF-8',
  };

  String get dbPath => p.join(dataDir, 'state.db');
  String get statusPath => p.join(dataDir, 'status.json');
  String get tokenPath => p.join(dataDir, 'token.json');

  void writeConfig({bool withClient = true}) {
    File(configPath).writeAsStringSync('''
${withClient ? '''sync_dir = ${jsonEncode(syncDir)}
drive_folder = "CloudDrop"
client_id = "123456-sandboxtest.apps.googleusercontent.com"
client_secret = "GOCSPX-sandbox-not-real"
''' : ''}parallel_uploads = 2
settle_secs = 1
rescan_secs = 600
notifications = false
chunk_size = 262144

[endpoints]
auth_url = "http://$mockAddr/auth"
token_url = "http://$mockAddr/token"
api_base = "http://$mockAddr/drive/v3"
upload_base = "http://$mockAddr/upload/drive/v3"
''');
  }

  void writeToken() {
    File(
      tokenPath,
    ).writeAsStringSync('{"access_token":"x","refresh_token":"good-refresh","expires_at":0}');
    Process.runSync('chmod', ['600', tokenPath]);
  }

  CliEngineApi api() => CliEngineApi(ProcessCliRunner(cliBin, environment: env));

  AppServices services(FakeSystem system) => AppServices(
    paths: AppPaths(
      configPath: configPath,
      dataDir: dataDir,
      defaultSyncDir: syncDir,
      cliBinary: cliBin,
      homeDir: home,
    ),
    engine: api(),
    db: SqliteStateReader.file(dbPath),
    status: FileStatusSource(statusPath),
    system: system,
    searchDelay: Duration.zero,
  );

  void dispose() {
    try {
      root.deleteSync(recursive: true);
    } catch (_) {}
  }
}

Future<Process> _spawn(String bin, List<String> args, Sandbox sb, String logName) async {
  final proc = await Process.start(
    bin,
    args,
    environment: sb.env,
    includeParentEnvironment: false,
    workingDirectory: sb.home,
  );
  final log = File(p.join(sb.root.path, logName)).openWrite();
  proc.stdout.listen(log.add, onDone: () {});
  proc.stderr.listen(
    log.add,
    onDone: () async {
      await log.flush();
    },
  );
  return proc;
}

Future<T> eventually<T>(
  FutureOr<T?> Function() probe, {
  String? what,
  Duration timeout = const Duration(seconds: 40),
  Duration every = const Duration(milliseconds: 150),
}) async {
  final end = DateTime.now().add(timeout);
  Object? lastError;
  while (DateTime.now().isBefore(end)) {
    try {
      final v = await probe();
      if (v != null && v != false) return v;
    } catch (e) {
      lastError = e;
    }
    await Future<void>.delayed(every);
  }
  fail(
    'timed out waiting for ${what ?? 'condition'}${lastError == null ? '' : ' (last error: $lastError)'}',
  );
}

void main() {
  if (!haveBinaries) {
    test(
      'real engine',
      () {},
      skip:
          'build the CLI first: cargo build -p clouddrop --example mock_drive && cargo build -p clouddrop',
    );
    return;
  }
  late Sandbox sb;
  late Process mock;
  late Process daemon;
  late SqliteStateReader reader;
  late FileStatusSource statusSource;
  late CliEngineApi api;
  late AppController app;
  late FakeSystem system;

  final random = Random(42);
  List<int> bytes(int n) => List<int>.generate(n, (_) => random.nextInt(256));

  setUpAll(() async {
    await loadTestFonts();
    sb = Sandbox('main');
    sb.writeConfig();
    sb.writeToken();
    Directory(sb.syncDir).createSync(recursive: true);

    // Fake Google Drive; 120 ms per upload chunk so progress is observable.
    mock = await _spawn(mockBin, [mockAddr, '120'], sb, 'mock.log');
    await eventually<bool>(() async {
      try {
        final s = await Socket.connect(
          '127.0.0.1',
          8299,
          timeout: const Duration(milliseconds: 200),
        );
        s.destroy();
        return true;
      } catch (_) {
        return null;
      }
    }, what: 'the fake Drive to listen on $mockAddr');

    daemon = await _spawn(cliBin, ['run'], sb, 'daemon.log');

    statusSource = FileStatusSource(sb.statusPath);
    reader = SqliteStateReader.file(sb.dbPath);
    api = sb.api();
    system = FakeSystem();
    app = AppController(sb.services(system));
  });

  tearDownAll(() async {
    reader.close();
    app.dispose();
    // Only the two processes started here.
    daemon.kill();
    mock.kill();
    await daemon.exitCode.timeout(
      const Duration(seconds: 10),
      onTimeout: () {
        daemon.kill(ProcessSignal.sigkill);
        return -1;
      },
    );
    await mock.exitCode.timeout(
      const Duration(seconds: 10),
      onTimeout: () {
        mock.kill(ProcessSignal.sigkill);
        return -1;
      },
    );
    sb.dispose();
  });

  test('the engine comes up and reports through status.json', () async {
    final s = await eventually<DaemonStatus>(() async {
      final st = await statusSource.read();
      return st != null && st.phase == Phase.idle ? st : null;
    }, what: 'the daemon to be idle');
    expect(s.pid, daemon.pid);
    expect(s.isLive(DateTime.now().millisecondsSinceEpoch), isTrue);
    expect(File(sb.dbPath).existsSync(), isTrue);
  });

  test('status --json and config get --json through the app\'s own CLI layer', () async {
    final st = await api.status();
    expect(st.configured, isTrue);
    expect(st.signedIn, isTrue, reason: 'token.json is there');
    expect(st.hasBuiltinClient, isFalse, reason: 'a source build has no Google client compiled in');
    expect(st.paused, isFalse);
    expect(st.folder, sb.syncDir);
    expect(st.daemon, isNotNull, reason: 'the daemon is alive');
    expect(st.daemon!.pid, daemon.pid);

    final cfg = (await api.configGet())!;
    expect(cfg.syncDir, sb.syncDir);
    expect(cfg.driveFolder, 'CloudDrop');
    expect(cfg.parallelUploads, 2);
    expect(cfg.settleSecs, 1);
    expect(cfg.notifications, isFalse);
    expect(cfg.autoFreeDays, isNull);
    expect(cfg.keepLocal, isEmpty);
    expect(cfg.hasClient, isTrue);
    expect(cfg.clientId, '123456-sandboxtest.apps.googleusercontent.com');
    expect(cfg.ignore, contains('*.part'));
  });

  test(
    'files dropped into the folder are uploaded; the app sees folders, files, activity and quota',
    () async {
      final dir = sb.syncDir;
      File(p.join(dir, 'hello.txt')).writeAsStringSync('hello from the sandbox\n');
      Directory(p.join(dir, 'Photos', 'Trip')).createSync(recursive: true);
      File(p.join(dir, 'Photos', 'beach.jpg')).writeAsBytesSync(bytes(300 * 1024));
      File(p.join(dir, 'Photos', 'Trip', 'map.png')).writeAsBytesSync(bytes(40 * 1024));
      Directory(p.join(dir, 'Docs', 'sub')).createSync(recursive: true);
      File(p.join(dir, 'Docs', 'sub', 'notes.md')).writeAsStringSync('# notes\n');
      File(p.join(dir, 'ignored.tmp')).writeAsStringSync('temporary files are ignored');

      await eventually<bool>(() async {
        final counts = await reader.counts();
        final synced = counts
            .where((c) => c.kind == 'file' && c.state == 'synced')
            .fold<int>(0, (s, c) => s + c.count);
        return synced == 4 ? true : null;
      }, what: 'four files to be uploaded');

      final top = await reader.listDir('');
      expect(top.map((i) => i.name), [
        'Docs',
        'Photos',
        'hello.txt',
      ], reason: 'folders first; the .tmp file is ignored');
      final photos = top[1];
      expect(photos.isDir, isTrue);
      expect(photos.stats!.files, 2);
      expect(photos.bytes, 340 * 1024);
      expect(photos.entry.driveId, isNotNull);
      expect((await reader.listDir('Photos')).map((i) => i.name), ['Trip', 'beach.jpg']);
      final hello = (await reader.entry('hello.txt'))!;
      expect(hello.state, EntryState.synced);
      expect(hello.size, 23);
      expect(hello.driveId, isNotNull);
      expect(hello.uploadedAt, isNotNull);

      // Search across folders.
      expect((await reader.search('MAP')).map((i) => i.path), ['Photos/Trip/map.png']);
      expect((await reader.search('notes')).map((i) => i.path), ['Docs/sub/notes.md']);

      // Activity: one "uploaded" per file, newest first.
      final acts = await reader.recentActivity(50);
      final uploaded = acts
          .where((a) => a.kind == ActivityKind.uploaded)
          .map((a) => a.path)
          .toSet();
      expect(uploaded, {
        'hello.txt',
        'Photos/beach.jpg',
        'Photos/Trip/map.png',
        'Docs/sub/notes.md',
      });
      expect(acts.first.id, greaterThan(acts.last.id));
      expect(await reader.lastActivityId(), acts.first.id);
      expect(
        await reader.meta('root_id'),
        isNotNull,
        reason: 'the top Drive folder id is recorded',
      );

      // The quota the daemon fetched from (the fake) about.get.
      final st = await eventually<DaemonStatus>(() async {
        final s = await statusSource.read();
        return s?.quota != null ? s : null;
      }, what: 'the daemon to publish the Drive quota');
      expect(st.quota!.limit, 16106127360);
      expect(st.quota!.usage, greaterThan(0));
    },
  );

  test('upload progress shows in status.json while a big file goes up', () async {
    final big = File(p.join(sb.syncDir, 'big.bin'))..writeAsBytesSync(bytes(3 * 1024 * 1024));
    Upload? seen;
    DaemonStatus? during;
    await eventually<bool>(
      () async {
        final s = await statusSource.read();
        if (s != null && s.uploads.isNotEmpty) {
          seen = s.uploads.first;
          during = s;
          return true;
        }
        return null;
      },
      what: 'an upload to show in status.json',
      every: const Duration(milliseconds: 40),
    );
    expect(seen!.path, 'big.bin');
    expect(seen!.total, 3 * 1024 * 1024);
    expect(seen!.sent, lessThanOrEqualTo(seen!.total));
    expect(during!.phase, anyOf(Phase.uploading, Phase.waiting));
    final snap = summarize(Snapshot(cfg: (await api.configGet())!, signedIn: true, daemon: during));
    expect(snap.title, startsWith('Uploading'));
    await eventually<bool>(
      () async => (await reader.entry('big.bin'))?.state == EntryState.synced ? true : null,
      what: 'big.bin to finish',
      timeout: const Duration(seconds: 60),
    );
    expect(big.existsSync(), isTrue);
  });

  test('the database changes while the engine works: data_version moves', () async {
    final before = await reader.dataVersion();
    File(p.join(sb.syncDir, 'tick.txt')).writeAsStringSync('x');
    await eventually<bool>(
      () async => (await reader.dataVersion()) != before ? true : null,
      what: 'data_version to change',
    );
    await eventually<bool>(
      () async => (await reader.entry('tick.txt'))?.state == EntryState.synced ? true : null,
      what: 'tick.txt to upload',
    );
  });

  group('the app\'s controllers over the real engine', () {
    test('AppController: one snapshot of everything', () async {
      await eventually<bool>(
        () async => (await statusSource.read())?.phase == Phase.idle ? true : null,
        what: 'the daemon to settle to idle',
      );
      await app.poll(forceEngine: true);
      expect(app.fatal, isNull);
      expect(app.setupActive, isFalse);
      final s = app.snapshot;
      expect(s.ready, isTrue);
      expect(s.counts.synced, 6);
      expect(s.counts.failed, 0);
      expect(s.daemon, isNotNull);
      expect(s.quota, isNotNull);
      expect(s.rootId, isNotNull);
      expect(s.root.files, 6);
      expect(s.cloudBytes, greaterThan(3 * 1024 * 1024));
      expect(s.paused, isFalse);
      final sum = summarize(s);
      expect(sum.title, 'Everything is uploaded');
      expect(sum.detail, startsWith('6 files · '));
    });

    test('FilesController: browse, search, filter, sort on the real database', () async {
      final c = FilesController(app)..visible = true;
      addTearDown(c.dispose);
      await c.reload();
      expect(c.items.map((i) => i.name), ['Docs', 'Photos', 'big.bin', 'hello.txt', 'tick.txt']);
      expect(c.stats!.files, 6);
      c.openDir('Photos');
      await c.reload();
      expect(c.items.map((i) => i.name), ['Trip', 'beach.jpg']);
      c.searchChanged('notes');
      await c.reload();
      expect(c.items.single.path, 'Docs/sub/notes.md');
      c.show('');
      await c.reload();
      c.setSort(const Sort(SortKey.size, true));
      expect(c.items.map((i) => i.name).skip(2).first, 'big.bin');
      c.setFilter(Filter.failed);
      await c.reload();
      expect(c.items, isEmpty);
    });

    test('ActivityController: history from the real engine', () async {
      final c = ActivityController(app);
      addTearDown(c.dispose);
      await c.refresh();
      expect(c.items.where((a) => a.kind == ActivityKind.uploaded).length, greaterThanOrEqualTo(6));
      expect(c.items.first.atMs, greaterThanOrEqualTo(c.items.last.atMs));
    });
  });

  group('CLI actions behave', () {
    test('config set: applied, validated, and visible in config get', () async {
      var r = await api.configSet('parallel_uploads', '3');
      expect((r.ok, r.message), (true, 'parallel_uploads updated.'));
      r = await api.configSet('settle_secs', '2');
      expect(r.ok, isTrue);
      r = await api.configSet('notifications', 'false');
      expect(r.ok, isTrue);
      r = await api.configSet('auto_free_days', 'now');
      expect(r.ok, isTrue);
      r = await api.configSet('ignore', '*.tmp, *.bak');
      expect(r.ok, isTrue);
      var cfg = (await api.configGet())!;
      expect((cfg.parallelUploads, cfg.settleSecs, cfg.autoFreeDays), (3, 2, 0));
      expect(cfg.ignore, ['*.tmp', '*.bak']);
      r = await api.configSet('auto_free_days', 'never');
      expect(r.ok, isTrue);
      r = await api.configSet('auto_free_days', '30');
      expect(r.ok, isTrue);
      cfg = (await api.configGet())!;
      expect(cfg.autoFreeDays, 30);
      await api.configSet('auto_free_days', 'never');
      expect((await api.configGet())!.autoFreeDays, isNull);

      final bad = await api.configSet('parallel_uploads', '0');
      expect(bad.ok, isFalse);
      expect(bad.message, 'parallel_uploads must be between 1 and 16');
      final unknown = await api.configSet('sync_dir', '/elsewhere');
      expect(unknown.ok, isFalse);
      expect(unknown.message, contains('unknown or read-only setting'));
      // Through the controller too: the new value shows at once.
      await app.poll(forceEngine: true);
      await app.setSetting('parallel_uploads', '2');
      expect(app.config!.parallelUploads, 2);
      await app.setSetting('ignore', '*.part, *.tmp, ~\$*');
    });

    test('pause stops uploading, resume continues', () async {
      var r = await api.pause();
      expect((r.ok, r.message), (true, 'Uploads paused.'));
      expect(await reader.meta('paused'), '1');
      expect((await api.status()).paused, isTrue, reason: 'status --json carries the flag');
      await eventually<bool>(
        () async => (await statusSource.read())?.phase == Phase.paused ? true : null,
        what: 'the daemon to report paused',
      );
      await app.poll(forceEngine: true);
      expect(app.snapshot.paused, isTrue);
      expect(summarize(app.snapshot).title, 'Uploads are paused');

      File(p.join(sb.syncDir, 'while-paused.txt')).writeAsStringSync('waiting');
      await eventually<bool>(
        () async => (await reader.entry('while-paused.txt')) != null ? true : null,
        what: 'the new file to be noticed',
      );
      await Future<void>.delayed(const Duration(seconds: 3));
      expect(
        (await reader.entry('while-paused.txt'))!.state,
        isNot(EntryState.synced),
        reason: 'not uploaded while paused',
      );

      r = await api.resume();
      expect((r.ok, r.message), (true, 'Uploads resumed.'));
      expect((await api.status()).paused, isFalse);
      await eventually<bool>(
        () async =>
            (await reader.entry('while-paused.txt'))?.state == EntryState.synced ? true : null,
        what: 'the file to upload after resume',
      );
      expect(await reader.meta('paused'), '0');
    });

    test('free removes the local copy, restore downloads it back', () async {
      final path = p.join(sb.syncDir, 'hello.txt');
      var r = await api.free([path]);
      expect(r.ok, isTrue, reason: r.message);
      expect(r.message.toLowerCase(), contains('freed'));
      expect(File(path).existsSync(), isFalse);
      var e = (await reader.entry('hello.txt'))!;
      expect(e.state, EntryState.cloudOnly);
      expect(e.size, 23, reason: 'the Drive copy size');
      expect(
        (await reader.listDir('')).firstWhere((i) => i.name == 'hello.txt').entry.state,
        EntryState.cloudOnly,
      );
      final freedAct = (await reader.recentActivity(
        5,
      )).firstWhere((a) => a.kind == ActivityKind.freed);
      expect(freedAct.path, 'hello.txt');

      // The Files controller sees it under Cloud-only, and the Home numbers follow.
      final c = FilesController(app)..visible = true;
      addTearDown(c.dispose);
      c.setFilter(Filter.cloudOnly);
      await c.reload();
      expect(c.items.map((i) => i.path), ['hello.txt']);
      await app.poll(forceEngine: true);
      expect(app.snapshot.counts.cloudOnly, 1);
      expect(app.snapshot.root.cloudOnlyBytes, 23);

      r = await api.restore([path]);
      expect(r.ok, isTrue, reason: r.message);
      expect(File(path).readAsStringSync(), 'hello from the sandbox\n');
      e = (await reader.entry('hello.txt'))!;
      expect(e.state, EntryState.synced);
      final restored = (await reader.recentActivity(
        5,
      )).firstWhere((a) => a.kind == ActivityKind.restored);
      expect(restored.path, 'hello.txt');
    });

    test(
      'free a whole folder; the controller does the same with the folder\'s absolute path',
      () async {
        await app.poll(forceEngine: true);
        final r = await app.free(['Photos']);
        expect(r.ok, isTrue, reason: r.message);
        expect(File(p.join(sb.syncDir, 'Photos', 'beach.jpg')).existsSync(), isFalse);
        expect(
          Directory(p.join(sb.syncDir, 'Photos', 'Trip')).existsSync(),
          isTrue,
          reason: 'folders are kept',
        );
        final photos = (await reader.listDir('')).firstWhere((i) => i.name == 'Photos');
        expect(photos.stats!.cloudOnly, 2);
        expect(photos.stats!.local, 0);
        final restore = await app.restore(['Photos'], '“Photos”');
        expect(restore.ok, isTrue, reason: restore.message);
        expect(File(p.join(sb.syncDir, 'Photos', 'beach.jpg')).existsSync(), isTrue);
        expect(File(p.join(sb.syncDir, 'Photos', 'Trip', 'map.png')).existsSync(), isTrue);
      },
    );

    test('paths outside the folder are refused with the engine\'s sentence', () async {
      final r = await api.free(['/definitely/not/inside']);
      expect(r.ok, isFalse);
      expect(r.message, contains('is not inside the CloudDrop folder'));
    });

    test('retry with nothing failed', () async {
      final r = await api.retry(const []);
      expect((r.ok, r.message), (true, 'No failed uploads.'));
    });

    test('keep and unkeep edit the pinned folders', () async {
      var r = await api.keep([p.join(sb.syncDir, 'Docs', 'sub')]);
      expect(r.ok, isTrue, reason: r.message);
      expect(r.message, contains('Docs/sub'));
      expect((await api.configGet())!.keepLocal, ['Docs/sub']);
      r = await api.keep([p.join(sb.syncDir, 'Photos')]);
      expect((await api.configGet())!.keepLocal, ['Docs/sub', 'Photos']);
      r = await api.unkeep([p.join(sb.syncDir, 'Docs', 'sub')]);
      expect(r.ok, isTrue);
      expect((await api.configGet())!.keepLocal, ['Photos']);
      // Through the controller (what the pin button does).
      await app.poll(forceEngine: true);
      await app.setKeep('Docs', true);
      expect((await api.configGet())!.keepLocal, ['Docs', 'Photos']);
      await app.setKeep('Docs', false);
      await app.setKeep('Photos', false);
      expect((await api.configGet())!.keepLocal, isEmpty);
      // Pinning the whole folder is refused.
      final root = await api.keep([sb.syncDir]);
      expect(root.ok, isFalse);
    });

    test('url prints the Drive link of an item', () async {
      final r = await api.driveUrl(p.join(sb.syncDir, 'hello.txt'));
      expect(r.ok, isTrue, reason: r.message);
      final id = (await reader.entry('hello.txt'))!.driveId!;
      expect(r.message, 'https://drive.google.com/file/d/$id/view');
      final root = await api.driveUrl(sb.syncDir);
      expect(
        root.message,
        'https://drive.google.com/drive/folders/${await reader.meta('root_id')}',
      );
    });

    test(
      'adding files copies them in without overwriting, and the engine uploads the copies',
      () async {
        Directory(p.join(sb.root.path, 'incoming')).createSync();
        final src = File(p.join(sb.root.path, 'incoming', 'hello.txt'))
          ..writeAsStringSync('a different hello\n');
        await app.poll(forceEngine: true);
        await app.copyIn('', [src.path]);
        expect(
          File(p.join(sb.syncDir, 'hello.txt')).readAsStringSync(),
          'hello from the sandbox\n',
        );
        expect(File(p.join(sb.syncDir, 'hello (2).txt')).readAsStringSync(), 'a different hello\n');
        await eventually<bool>(
          () async =>
              (await reader.entry('hello (2).txt'))?.state == EntryState.synced ? true : null,
          what: 'the copy to be uploaded',
        );
      },
    );

    test(
      'a file removed from disk stays listed as cloud-only (upload-only: Drive is never touched)',
      () async {
        File(p.join(sb.syncDir, 'tick.txt')).deleteSync();
        await eventually<bool>(
          () async => (await reader.entry('tick.txt'))?.state == EntryState.cloudOnly ? true : null,
          what: 'tick.txt to become cloud-only',
        );
        final c = FilesController(app)..visible = true;
        addTearDown(c.dispose);
        c.setFilter(Filter.cloudOnly);
        await c.reload();
        expect(c.items.map((i) => i.path), contains('tick.txt'));
      },
    );
  });

  testWidgets(
    'the screens over the real engine: Home, Files and Activity show what the daemon did',
    (tester) async {
      tester.useWindow(wide);
      final real = AppController(sb.services(FakeSystem()));
      addTearDown(real.dispose);
      await tester.runAsync(() async {
        await eventually<bool>(
          () async => (await statusSource.read())?.phase == Phase.idle ? true : null,
          what: 'the daemon to be idle',
        );
        await real.poll(forceEngine: true);
      });
      await tester.pumpWidget(
        CloudDropApp(
          app: real,
          fontFamily: testFont,
          fontFamilyFallback: const [testFallback],
          themeMode: ThemeMode.light,
          enableDrop: false,
        ),
      );
      await tester.settle();
      expect(find.text('Everything is uploaded'), findsOneWidget);
      expect(find.textContaining('files · '), findsWidgets);
      expect(
        find.textContaining('of 15.0 GB used'),
        findsOneWidget,
        reason: 'the quota the daemon fetched',
      );

      // Files: the real tree, cloud-only items included.
      real.showFiles();
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 600)));
      await tester.settle();
      for (final name in [
        'Docs',
        'Photos',
        'big.bin',
        'hello.txt',
        'hello (2).txt',
        'tick.txt',
        'while-paused.txt',
      ]) {
        expect(find.text(name), findsOneWidget, reason: name);
      }
      expect(find.text('Cloud-only'), findsWidgets, reason: 'tick.txt was removed from disk');
      expect(find.text('Uploaded'), findsWidgets);
      expect(find.text('3.0 MB'), findsOneWidget, reason: 'big.bin');

      // Activity: uploads, newest first.
      real.pane.value = AppPane.activity;
      await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 300)));
      await tester.settle();
      expect(find.text('Uploaded big.bin'), findsOneWidget);
      expect(find.text('Freed up hello.txt'), findsOneWidget);
      expect(find.text('Downloaded hello.txt'), findsOneWidget);
      expect(find.text('Today'), findsWidgets);
      await tester.pumpWidget(const SizedBox());
      await tester.pump(const Duration(seconds: 1));
    },
  );

  group('first run in a fresh sandbox: setup, sign-in (fake Google), done', () {
    late Sandbox fresh;
    setUp(() {
      fresh = Sandbox('fresh');
    });
    tearDown(() => fresh.dispose());

    test(
      'no config yet: status says so; setup writes the config; login --no-browser completes through the fake Google',
      () async {
        final api = fresh.api();

        // 1. Nothing exists: status --json works anyway (one call) and says what the assistant needs.
        var st = await api.status();
        expect(st.configured, isFalse);
        expect(st.signedIn, isFalse);
        expect(
          st.hasBuiltinClient,
          isFalse,
          reason: 'a source build has no Google client compiled in',
        );
        expect(st.daemon, isNull);
        expect(st.paused, isFalse);
        expect(st.rootId, isNull);
        expect(File(fresh.configPath).existsSync(), isFalse, reason: 'asking creates nothing');
        expect(Directory(fresh.dataDir).listSync(), isEmpty);
        expect(await api.configGet(), isNull);

        // 2. A developer's config that only redirects Google to the fake (so sign-in stays local).
        fresh.writeConfig(withClient: false);
        File(fresh.configPath).writeAsStringSync(
          File(fresh.configPath).readAsStringSync().replaceAll('\nchunk_size = 262144\n', '\n'),
        );

        // 3. Setup with a pasted key travels through a temp file.
        final folder = p.join(fresh.home, 'My Cloud Folder');
        final setup = await api.setup(
          folder: folder,
          driveFolder: 'Fresh Folder',
          clientId: '777-fresh.apps.googleusercontent.com',
          clientSecret: 'GOCSPX-fresh',
        );
        expect(setup.ok, isTrue, reason: setup.message);
        expect(Directory(folder).existsSync(), isTrue, reason: 'the folder is created');
        final cfg = (await api.configGet())!;
        expect(cfg.syncDir, folder);
        expect(cfg.driveFolder, 'Fresh Folder');
        expect(cfg.clientId, '777-fresh.apps.googleusercontent.com');
        expect(cfg.hasClientSecret, isTrue);
        st = await api.status();
        expect(st.configured, isTrue);
        expect(st.signedIn, isFalse, reason: 'configured but not signed in yet');

        // 4. login --no-browser: the app gets the link, "opens" it (an HTTP GET to the fake consent page).
        final session = api.login();
        final url = await session.url.timeout(const Duration(seconds: 20));
        expect(url, startsWith('http://$mockAddr/auth?'));
        // (The widget-test binding fakes every HttpClient with status 400; this one is real.)
        final overrides = HttpOverrides.current;
        HttpOverrides.global = null;
        final client = HttpClient();
        HttpOverrides.global = overrides;
        final req = await client.getUrl(Uri.parse(url));
        req.followRedirects = true;
        final resp = await req.close();
        await resp.drain<void>();
        client.close();
        final done = await session.done.timeout(const Duration(seconds: 20));
        expect(done.ok, isTrue, reason: done.message);
        expect(File(fresh.tokenPath).existsSync(), isTrue);
        st = await api.status();
        expect(st.signedIn, isTrue);
        expect(st.ready, isTrue);

        // 5. A cancelled sign-in leaves nothing running.
        final again = api.login();
        await again.url.timeout(const Duration(seconds: 20));
        again.cancel();
        final r = await again.done.timeout(const Duration(seconds: 10));
        expect(r.ok, isFalse);
        expect(r.message, 'Sign-in cancelled.');
        expect(
          (await api.status()).signedIn,
          isTrue,
          reason: 'a cancelled sign-in keeps the old one',
        );

        // 6. Sign out: the sign-in is forgotten, the configuration stays (no daemon runs here,
        // so there is no service to stop).
        final out = await api.logout();
        expect(out.ok, isTrue, reason: out.message);
        expect(out.message, startsWith('Signed out.'));
        expect(File(fresh.tokenPath).existsSync(), isFalse);
        st = await api.status();
        expect((st.configured, st.signedIn, st.ready), (true, false, false));
        expect((await api.configGet())!.syncDir, folder);
        // ...and signing out twice is harmless.
        expect((await api.logout()).ok, isTrue);
      },
    );

    test('AppController.signOut over the real engine leads back to the sign-in step', () async {
      fresh.writeConfig();
      fresh.writeToken();
      Directory(fresh.syncDir).createSync(recursive: true);
      final real = AppController(fresh.services(FakeSystem()));
      addTearDown(real.dispose);
      await real.poll(forceEngine: true);
      expect(real.setupActive, isFalse, reason: 'configured and signed in');
      expect(real.snapshot.signedIn, isTrue);
      final r = await real.signOut();
      expect(r.ok, isTrue, reason: r.message);
      expect(File(fresh.tokenPath).existsSync(), isFalse);
      expect(real.snapshot.signedIn, isFalse);
      expect(real.setupActive, isTrue);
      expect(real.setupAtSignIn, isTrue);
    });

    test('status --json: paused and the Drive folder id of a configured sandbox', () async {
      fresh.writeConfig();
      fresh.writeToken();
      Directory(fresh.syncDir).createSync(recursive: true);
      final api = fresh.api();
      var st = await api.status();
      expect((st.configured, st.signedIn, st.paused), (true, true, false));
      expect(st.rootId, isNull, reason: 'nothing uploaded yet, no Drive folder');
      expect((await api.pause()).ok, isTrue);
      st = await api.status();
      expect(st.paused, isTrue);
      expect((await api.resume()).ok, isTrue);
      expect((await api.status()).paused, isFalse);
    });

    test(
      'an unreadable config shows up as "not set up" (the engine can\'t tell the difference)',
      () async {
        fresh.writeConfig(withClient: false);
        File(fresh.configPath).writeAsStringSync('this is = not [valid toml');
        final api = fresh.api();
        final st = await api.status();
        expect(st.configured, isFalse);
        expect(await api.configGet(), isNull);
      },
    );
  });

  test('the CLI that is not there is reported cleanly', () async {
    final api = CliEngineApi(ProcessCliRunner(p.join(sb.root.path, 'no-such-clouddrop')));
    expect(api.status(), throwsA(isA<CliUnavailable>()));
    final r = await api.pause();
    expect(r.ok, isFalse);
    expect(r.message, contains('no-such-clouddrop'));
  });
}
