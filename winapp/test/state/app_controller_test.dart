import 'dart:io';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:clouddrop_app/logic/snapshot.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

import '../support/fakes.dart';

DaemonStatus daemon({
  Phase phase = Phase.idle,
  int waiting = 0,
  List<Upload> uploads = const [],
  Quota? quota,
}) => DaemonStatus(
  pid: 1,
  updatedMs: 1,
  phase: phase,
  waiting: waiting,
  uploads: uploads,
  quota: quota,
);

void main() {
  test('reads the engine, status.json and the database into one snapshot', () async {
    final t = TestApp(daemon: daemon(quota: const Quota(limit: 100, usage: 10, usageInDrive: 5)));
    t.db.file('a.txt', size: 100);
    t.db.file('b.txt', state: 'cloud_only', cloudSize: 300);
    t.db.file('c.txt', state: 'error', size: 5, error: 'boom');
    t.db.file('d.txt', state: 'pending', size: 7);
    t.db.meta('root_id', 'ROOT');
    final app = await t.ready();
    final s = app.snapshot;
    expect(app.loaded, isTrue);
    expect(s.ready, isTrue);
    expect((s.counts.synced, s.counts.cloudOnly, s.counts.failed, s.counts.waiting), (1, 1, 1, 1));
    expect(s.cloudBytes, 400);
    expect(s.cloudOnlyBytes, 300);
    expect(s.root.files, 4);
    expect(s.rootId, 'ROOT');
    expect(s.quota!.limit, 100);
    expect(s.paused, isFalse);
    expect(app.syncDir, '/home/me/CloudDrop');
    expect(
      app.setupActive,
      isFalse,
      reason: 'configured and signed in: straight to the main window',
    );
  });

  test('pause shows up from the database', () async {
    final t = TestApp(daemon: daemon());
    t.db.meta('paused', '1');
    final app = await t.ready();
    expect(app.snapshot.paused, isTrue);
  });

  test('not configured: the setup assistant is due, with the built-in client flag', () async {
    final t = TestApp(status: const EngineStatus.unconfigured(hasBuiltinClient: true));
    t.engine.config = null;
    final app = await t.ready();
    expect(app.setupActive, isTrue);
    expect(app.hasBuiltinClient, isTrue);
    expect(app.snapshot.cfg, isNull);
    expect(summarize(app.snapshot).action, SummaryAction.setup);
  });

  test('configured but not signed in: the assistant is due', () async {
    final t = TestApp(status: const EngineStatus(configured: true, signedIn: false));
    final app = await t.ready();
    expect(app.setupActive, isTrue);
    expect(app.snapshot.signedIn, isFalse);
  });

  test('the engine\'s "configured" decides, even when a config file exists', () async {
    final t = TestApp(
      config: const AppConfig(
        syncDir: '/s',
        driveFolder: 'D',
        clientId: '',
        hasClientSecret: false,
      ),
      status: const EngineStatus.unconfigured(hasBuiltinClient: true),
    );
    final app = await t.ready();
    expect(app.setupActive, isTrue);
    expect(app.snapshot.cfg, isNull);
    expect(app.config!.syncDir, '/s', reason: 'still there, as defaults for the assistant');
  });

  test(
    'paused comes from the database flag; the engine status covers an unreadable database',
    () async {
      final t = TestApp(
        daemon: daemon(),
        status: const EngineStatus(configured: true, signedIn: true, paused: true),
      );
      // Readable database, no flag: the database (always the freshest) says not paused.
      final app = await t.ready();
      expect(app.snapshot.paused, isFalse);

      final t2 = TestApp(
        daemon: daemon(),
        status: const EngineStatus(configured: true, signedIn: true, paused: true),
        reader: SqliteStateReader.file('/nonexistent/clouddrop/state.db'),
      );
      final app2 = await t2.ready();
      expect(app2.snapshot.paused, isTrue, reason: 'no database to read: status --json decides');
    },
  );

  test('the Drive folder id from status wins over the database', () async {
    final t = TestApp(
      daemon: daemon(),
      status: const EngineStatus(configured: true, signedIn: true, rootId: 'FROM-STATUS'),
    );
    t.db.meta('root_id', 'FROM-DB');
    expect((await t.ready()).snapshot.rootId, 'FROM-STATUS');

    final t2 = TestApp(daemon: daemon());
    t2.db.meta('root_id', 'FROM-DB');
    expect((await t2.ready()).snapshot.rootId, 'FROM-DB', reason: 'status has none: the database');
  });

  group('a settings file that exists but cannot be read', () {
    const error =
        'invalid config /home/me/.config/clouddrop/config.toml: TOML parse error at line 1, column 6\n'
        '  |\n1 | this is = not [valid toml\n  |      ^\nkey with no value, expected `=`\n';

    test('never opens the setup assistant (it would overwrite the file)', () async {
      final t = TestApp(status: const EngineStatus.unconfigured(configError: error));
      t.engine.config = null;
      final app = await t.ready();
      expect(app.loaded, isTrue);
      expect(app.configError, error);
      expect(app.setupActive, isFalse);
      expect(app.fatal, isNull);
      expect(t.engine.calls, isEmpty, reason: 'nothing was written or run');
    });

    test('Open Settings Folder opens the folder that holds config.toml', () async {
      final t = TestApp(status: const EngineStatus.unconfigured(configError: error));
      t.engine.config = null;
      final app = await t.ready();
      app.openSettingsFolder();
      expect(t.system.opened, ['/home/me/.config/clouddrop']);
    });

    test('Try Again re-reads: once the file is fixed the normal start-up check runs', () async {
      final t = TestApp(status: const EngineStatus.unconfigured(configError: error));
      t.engine.config = null;
      final app = await t.ready();
      // Still broken: nothing changes.
      await app.refreshNow();
      expect(app.configError, error);
      expect(app.setupActive, isFalse);
      // Fixed, but never set up: now the assistant is due.
      t.engine.statusValue = const EngineStatus.unconfigured(hasBuiltinClient: true);
      await app.refreshNow();
      expect(app.configError, isNull);
      expect(app.setupActive, isTrue);
    });

    test('fixed and already set up: straight to the main window', () async {
      final t = TestApp(status: const EngineStatus.unconfigured(configError: error));
      final app = await t.ready();
      expect(app.setupActive, isFalse);
      t.engine.statusValue = const EngineStatus(
        configured: true,
        signedIn: true,
        folder: '/home/me/CloudDrop',
      );
      await app.refreshNow();
      expect(app.configError, isNull);
      expect(app.setupActive, isFalse);
      expect(app.snapshot.ready, isTrue);
    });

    test('a file that breaks while the app is running shows the error too', () async {
      final t = TestApp(daemon: daemon());
      final app = await t.ready();
      expect(app.configError, isNull);
      var heard = 0;
      app.addListener(() => heard++);
      t.engine.statusValue = const EngineStatus.unconfigured(configError: error);
      await app.refreshNow();
      expect(app.configError, error);
      expect(heard, greaterThan(0));
      expect(app.setupActive, isFalse);
    });
  });

  test('finishing the setup returns to the main window', () async {
    final t = TestApp(status: const EngineStatus(configured: true, signedIn: false));
    final app = await t.ready();
    expect(app.setupActive, isTrue);
    t.engine.statusValue = const EngineStatus(
      configured: true,
      signedIn: true,
      folder: '/home/me/CloudDrop',
    );
    // Even once signed in, the assistant stays until the user finishes it.
    await app.poll(forceEngine: true);
    expect(app.setupActive, isTrue);
    await app.finishSetup();
    expect(app.setupActive, isFalse);
    expect(app.snapshot.ready, isTrue);
  });

  test('an engine that cannot be run is reported, not hidden', () async {
    final t = TestApp();
    t.engine.statusError = StateError('Couldn\'t run clouddrop: not found');
    final app = await t.ready();
    expect(app.fatal, contains('not found'));
    expect(app.setupActive, isFalse);
  });

  test(
    'one failed answer after startup keeps the screen; a few in a row explain themselves',
    () async {
      final t = TestApp(daemon: daemon());
      final app = await t.ready();
      expect(app.fatal, isNull);
      t.engine.statusError = StateError('database is locked');
      await app.poll(forceEngine: true);
      await app.poll(forceEngine: true);
      expect(app.fatal, isNull, reason: 'two hiccups are not an outage');
      expect(app.snapshot.ready, isTrue);
      await app.poll(forceEngine: true);
      expect(app.fatal, 'database is locked');
      expect(app.fatalIsMissingCli, isFalse);
      t.engine.statusError = null;
      await app.poll(forceEngine: true);
      expect(app.fatal, isNull);
      t.engine.statusError = StateError('again');
      await app.poll(forceEngine: true);
      expect(app.fatal, isNull, reason: 'the count starts over after a good answer');
    },
  );

  test('an engine that was down at the start still gets the setup check once it answers', () async {
    final t = TestApp(status: const EngineStatus.unconfigured(hasBuiltinClient: true));
    t.engine.config = null;
    t.engine.statusError = CliUnavailable('clouddrop', 'not found');
    final app = await t.ready();
    expect(app.fatal, contains('not found'));
    expect(app.fatalIsMissingCli, isTrue);
    expect(app.setupActive, isFalse);
    t.engine.statusError = null;
    await app.poll(forceEngine: true);
    expect(app.fatal, isNull);
    expect(app.setupActive, isTrue, reason: 'unconfigured: the assistant starts');
  });

  test('listeners hear about changes, not about identical polls', () async {
    final t = TestApp(daemon: daemon());
    final app = await t.ready();
    var n = 0;
    app.addListener(() => n++);
    await app.poll();
    await app.poll();
    expect(n, 0);
    t.statusSource.value = daemon(phase: Phase.uploading, waiting: 2);
    await app.poll();
    expect(n, 1);
    expect(app.snapshot.daemon!.phase, Phase.uploading);
  });

  test('dbVersion bumps when the database changes, and the numbers follow', () async {
    final t = TestApp(daemon: daemon());
    t.db.file('a.txt', size: 1);
    final app = await t.ready();
    final v = app.dbVersion.value;
    await app.poll();
    expect(
      app.dbVersion.value,
      v,
      reason: 'nothing changed: no reload for the Files and Activity screens',
    );
    expect(app.snapshot.counts.synced, 1);
    t.db.file('b.txt', size: 2);
    await app.poll();
    expect(app.dbVersion.value, v + 1);
    expect(app.snapshot.counts.synced, 2);
    expect(app.snapshot.root.bytes, 3);
  });

  test('the aggregates are not recomputed on every tick', () async {
    final t = TestApp(daemon: daemon());
    t.db.file('a.txt', size: 1);
    final app = await t.ready();
    // Change the data behind the reader's back (without moving data_version): a quiet poll must
    // not rescan, so the old numbers stay until the database says it changed.
    t.db.db.execute("UPDATE entries SET state = 'error' WHERE path = 'a.txt'");
    // (total_changes moved, so this is a change as far as the app can tell)
    await app.poll();
    expect(app.snapshot.counts.failed, 1);
  });

  test('paused follows both the database flag and the engine phase', () async {
    final t = TestApp(daemon: daemon(phase: Phase.paused));
    final app = await t.ready();
    expect(app.snapshot.paused, isTrue);
    t.statusSource.value = daemon(phase: Phase.idle);
    await app.poll();
    expect(app.snapshot.paused, isFalse, reason: 'the engine resumed');
    t.db.meta('paused', '1');
    await app.poll();
    expect(app.snapshot.paused, isTrue);
  });

  group('actions go through the CLI with absolute paths', () {
    late TestApp t;
    late AppController app;
    late List<AppMessage> messages;

    setUp(() async {
      t = TestApp(daemon: daemon());
      app = await t.ready();
      messages = [];
      app.messages.listen(messages.add);
    });

    Future<void> settle() => Future<void>.delayed(Duration.zero);

    test('free, restore, retry', () async {
      await app.free(['Photos', 'a.txt']);
      await app.free(['']);
      await app.restore(['Photos/b.jpg'], '“b.jpg”');
      await app.retry(['Photos']);
      await app.retry(['']);
      expect(t.engine.calls, [
        'free /home/me/CloudDrop/Photos /home/me/CloudDrop/a.txt',
        'free /home/me/CloudDrop',
        'restore /home/me/CloudDrop/Photos/b.jpg',
        'retry /home/me/CloudDrop/Photos',
        'retry',
      ]);
      await settle();
      expect(messages.map((m) => m.text), contains('Downloading “b.jpg”…'));
      expect(
        messages.map((m) => m.text),
        contains('Done: free /home/me/CloudDrop/Photos /home/me/CloudDrop/a.txt'),
      );
    });

    test('errors from the engine reach the user as errors', () async {
      t.engine.result = (c) => const ActionResult(false, 'Drive says no');
      await app.free(['x']);
      await settle();
      expect(messages.last.text, 'Drive says no');
      expect(messages.last.severity, Severity.error);
    });

    test('busy while an action runs', () async {
      expect(app.busy, isFalse);
      final seen = <bool>[];
      app.busyCount.addListener(() => seen.add(app.busy));
      await app.free(['x']);
      expect(seen, [true, false]);
    });

    test('pause, resume, keep, unkeep', () async {
      await app.setPaused(true);
      await app.setPaused(false);
      await app.setKeep('Work', true);
      await app.setKeep('Work', false);
      expect(t.engine.calls, [
        'pause',
        'resume',
        'keep /home/me/CloudDrop/Work',
        'unkeep /home/me/CloudDrop/Work',
      ]);
    });

    test('settings are applied live and shown at once', () async {
      final r = await app.setSetting('parallel_uploads', '4');
      expect(r.ok, isTrue);
      expect(t.engine.calls, ['config set parallel_uploads 4']);
      expect(app.config!.parallelUploads, 4);
      await app.setSetting('auto_free_days', 'now');
      expect(app.config!.autoFreeDays, 0);
      await app.setSetting('auto_free_days', 'never');
      expect(app.config!.autoFreeDays, isNull);
      await app.setSetting('auto_free_days', '30');
      expect(app.config!.autoFreeDays, 30);
      await app.setSetting('notifications', 'false');
      expect(app.config!.notifications, isFalse);
      await app.setSetting('ignore', '*.a, *.b');
      expect(app.config!.ignore, ['*.a', '*.b']);
    });

    test('a rejected setting says why', () async {
      t.engine.result = (c) =>
          const ActionResult(false, 'parallel_uploads must be between 1 and 16');
      await app.setSetting('parallel_uploads', '99');
      await settle();
      expect(messages.last.text, "Couldn't save: parallel_uploads must be between 1 and 16");
      expect(app.config!.parallelUploads, 2, reason: 'unchanged');
    });

    test('start at sign-in and the service', () async {
      await app.setStartAtSignIn(true);
      await app.setStartAtSignIn(false);
      await app.startService();
      expect(t.engine.calls, ['service enable', 'service disable', 'service start']);
    });
  });

  group('opening things', () {
    late TestApp t;
    late AppController app;
    setUp(() async {
      t = TestApp(daemon: daemon());
      t.db.meta('root_id', 'ROOT');
      app = await t.ready();
    });

    test('open folder / file', () {
      app.openFolder();
      app.openLocal('Photos/a.jpg');
      expect(t.system.opened, ['/home/me/CloudDrop', '/home/me/CloudDrop/Photos/a.jpg']);
    });

    test('Drive links for files and folders come from the database ids', () async {
      await app.openInDrive(
        const Entry(path: 'a', kind: EntryKind.file, state: EntryState.synced, driveId: 'F1'),
      );
      await app.openInDrive(
        const Entry(path: 'd', kind: EntryKind.dir, state: EntryState.synced, driveId: 'D1'),
      );
      await app.openInDrive(
        const Entry(path: 'p', kind: EntryKind.file, state: EntryState.pending),
      );
      expect(t.system.urls, [
        'https://drive.google.com/file/d/F1/view',
        'https://drive.google.com/drive/folders/D1',
      ]);
    });

    test('the top folder link is what `clouddrop url` prints', () async {
      await app.openRootInDrive();
      expect(t.engine.calls, ['url /home/me/CloudDrop']);
      expect(t.system.urls, ['https://drive.google.com/drive/folders/ROOT']);
    });

    test('...falling back to the recorded folder id when the CLI cannot say', () async {
      t.engine.result = (c) => const ActionResult(false, 'not uploaded yet');
      await app.openRootInDrive();
      expect(t.system.urls, ['https://drive.google.com/drive/folders/ROOT']);
    });

    test('...and says why when there is neither', () async {
      final t2 = TestApp(daemon: daemon());
      t2.engine.result = (c) => const ActionResult(false, 'not uploaded yet');
      final app2 = await t2.ready();
      final msgs = <AppMessage>[];
      app2.messages.listen(msgs.add);
      await app2.openRootInDrive();
      await Future<void>.delayed(Duration.zero);
      expect(t2.system.urls, isEmpty);
      expect(msgs.single.text, 'not uploaded yet');
      expect(msgs.single.severity, Severity.error);
    });
  });

  test(
    'Show in Explorer selects the file, or opens the nearest folder for cloud-only items',
    () async {
      final tmp = Directory.systemTemp.createTempSync('clouddrop-show');
      addTearDown(() => tmp.deleteSync(recursive: true));
      Directory(p.join(tmp.path, 'Photos')).createSync();
      File(p.join(tmp.path, 'Photos', 'a.jpg')).writeAsStringSync('x');
      final t = TestApp(syncDir: tmp.path, daemon: daemon());
      final app = await t.ready();
      app.showInExplorer('Photos/a.jpg');
      app.showInExplorer('Photos/gone/deeper/b.jpg');
      app.showInExplorer('missing-top.txt');
      expect(t.system.revealed, [
        p.join(tmp.path, 'Photos', 'a.jpg'),
        p.join(tmp.path, 'Photos'),
        tmp.path,
      ]);
    },
  );

  group('sign out', () {
    test('logout, then the assistant opens on its sign-in page', () async {
      final t = TestApp(daemon: daemon());
      final app = await t.ready();
      expect(app.setupActive, isFalse);
      // After `clouddrop logout` the engine reports no sign-in.
      t.engine.result = (c) {
        if (c == 'logout') {
          t.engine.statusValue = const EngineStatus(
            configured: true,
            signedIn: false,
            folder: '/home/me/CloudDrop',
          );
        }
        return const ActionResult(true, 'Signed out. Uploads stop until you sign in again.');
      };
      final msgs = <AppMessage>[];
      app.messages.listen(msgs.add);
      final r = await app.signOut();
      expect(r.ok, isTrue);
      expect(t.engine.calls, ['logout']);
      expect(app.snapshot.signedIn, isFalse);
      expect(app.setupActive, isTrue);
      expect(app.setupAtSignIn, isTrue);
      await Future<void>.delayed(Duration.zero);
      expect(msgs.single.text, startsWith('Signed out.'));
    });

    test('a failed logout stays where it is and says why', () async {
      final t = TestApp(daemon: daemon());
      final app = await t.ready();
      t.engine.result = (c) => const ActionResult(false, 'could not stop the service');
      final msgs = <AppMessage>[];
      app.messages.listen(msgs.add);
      final r = await app.signOut();
      expect(r.ok, isFalse);
      expect(app.setupActive, isFalse);
      await Future<void>.delayed(Duration.zero);
      expect(msgs.single.severity, Severity.error);
      expect(msgs.single.text, 'could not stop the service');
    });
  });

  group('Windows placeholders (a freed file stays on disk and downloads when opened)', () {
    test('only where the sync folder is a Cloud Files sync root', () async {
      final win = TestApp(daemon: daemon(), cloudFiles: true);
      win.onDisk.add('/home/me/CloudDrop/a.txt');
      final app = await win.ready();
      expect(await app.isPlaceholder('a.txt'), isTrue);
      expect(await app.isPlaceholder('missing.txt'), isFalse);

      final linux = TestApp(daemon: daemon());
      linux.onDisk.add('/home/me/CloudDrop/a.txt');
      expect(
        await (await linux.ready()).isPlaceholder('a.txt'),
        isFalse,
        reason: 'Linux: freed = gone',
      );
    });
  });

  test('navigation request for the Files screen', () async {
    final t = TestApp(daemon: daemon());
    final app = await t.ready();
    app.showFiles(filter: Filter.failed);
    expect(app.pane.value, AppPane.files);
    expect(app.filesNav.value!.filter, Filter.failed);
    app.showFiles(dir: 'Photos', select: 'Photos/a.jpg');
    expect(app.filesNav.value!.select, 'Photos/a.jpg');
  });
}
