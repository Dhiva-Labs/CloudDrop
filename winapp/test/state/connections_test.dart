/// Several folders (connections) in the controllers: the list, the selection (remembered in
/// app.json, with a fallback), `--connection` on every command, each folder's status for the
/// switcher, adding a folder (its name, Cancel taking it back) and removing one.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/providers.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:clouddrop_app/logic/snapshot.dart';
import 'package:clouddrop_app/state/activity_controller.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/state/files_controller.dart';
import 'package:clouddrop_app/state/setup_controller.dart';
import 'package:clouddrop_app/ui/app.dart' show windowTitle;
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/scenarios.dart';
import '../support/test_db.dart';

DaemonStatus heartbeat(Phase phase, {int waiting = 0, List<Upload> uploads = const []}) =>
    DaemonStatus(pid: 7, updatedMs: 1, phase: phase, waiting: waiting, uploads: uploads);

/// The default folder (up to date, a few files) and "Work" (uploading, files of its own).
Future<(TestApp, TestConnection)> twoFolders({String? remember}) async {
  final t = TestApp(daemon: heartbeat(Phase.idle));
  t.db.file('home.txt', size: 10);
  t.db.activity('uploaded', 'home.txt', at: 1, bytes: 10);
  final work = t.addConnection(
    'Work',
    daemon: heartbeat(
      Phase.uploading,
      waiting: 2,
      uploads: const [Upload(path: 'plan.pptx', sent: 1, total: 4)],
    ),
  );
  work.db.dir('Plans');
  work.db.file('Plans/plan.pptx', state: 'uploading', size: 4);
  work.db.file('contract.pdf', size: 300, uploadedAt: 1);
  work.db.file('broken.bin', state: 'error', size: 5, error: 'boom');
  work.db.activity('uploaded', 'contract.pdf', at: 2, bytes: 300);
  t.prefs.remembered = remember;
  await t.ready();
  return (t, work);
}

/// A reader that remembers being told to let go of its file.
class ReleaseRecorder implements StateReader {
  ReleaseRecorder(this.inner);
  final StateReader inner;
  int released = 0;

  @override
  void release() => released++;
  @override
  Future<Listing> browse(Request req) => inner.browse(req);
  @override
  Future<List<DirItem>> listDir(String dir) => inner.listDir(dir);
  @override
  Future<List<DirItem>> search(String needle, {int limit = searchLimit}) =>
      inner.search(needle, limit: limit);
  @override
  Future<Entry?> entry(String path) => inner.entry(path);
  @override
  Future<FolderStats> folderStats(String dir) => inner.folderStats(dir);
  @override
  Future<List<StateCount>> counts() => inner.counts();
  @override
  Future<CloudBytes> cloudBytes() => inner.cloudBytes();
  @override
  Future<List<Activity>> recentActivity(int limit) => inner.recentActivity(limit);
  @override
  Future<int> lastActivityId() => inner.lastActivityId();
  @override
  Future<String?> meta(String key) => inner.meta(key);
  @override
  Future<int?> dataVersion() => inner.dataVersion();
  @override
  void close() => inner.close();
}

void main() {
  group('one folder: as it always was', () {
    test(
      'no switcher; the top is "CloudDrop"; every command is the default connection\'s',
      () async {
        final t = TestApp(syncDir: '/home/me/Stuff', daemon: heartbeat(Phase.idle));
        final app = await t.ready();
        expect(app.connectionsKnown, isTrue);
        expect(app.connections.single.name, 'default');
        expect(app.connectionName, 'default');
        expect(app.several, isFalse);
        expect(app.switcher, isEmpty);
        expect(app.label, 'Stuff', reason: "the default connection's label is its folder's name");
        expect(app.rootName, 'CloudDrop', reason: 'one folder reads as it always did');
        expect(windowTitle(app), 'CloudDrop');
        await app.setPaused(true);
        expect(t.engine.calls, ['pause']);
        expect(t.engine.log, ['default: pause']);
        expect(t.prefs.remembered, isNull, reason: 'nothing to remember with one folder');
      },
    );

    test(
      'an engine from before connections: no list, no --connection, nothing else changes',
      () async {
        final t = TestApp(daemon: heartbeat(Phase.idle));
        t.engine.connectionsSupported = false;
        t.db.file('a.txt', size: 1);
        final app = await t.ready();
        expect(app.connectionsKnown, isFalse);
        expect(app.connections, isEmpty);
        expect(identical(app.engine, t.engine), isTrue);
        expect(app.snapshot.ready, isTrue);
        expect(app.snapshot.counts.synced, 1);
        expect(app.switcher, isEmpty);
      },
    );

    test(
      'before the first setup there is no connection: the assistant sets up the default one',
      () async {
        final t = TestApp(status: const EngineStatus.unconfigured());
        t.engine.config = null;
        final app = await t.ready();
        expect(app.connections, isEmpty);
        expect(app.connectionName, 'default');
        expect(app.setupActive, isTrue);
        final c = SetupController(app);
        addTearDown(c.dispose);
        expect(c.adding, isFalse);
        expect(c.steps.first, SetupStep.welcome);
        expect(c.title, 'Set up CloudDrop');
      },
    );
  });

  group('several folders', () {
    test('the switcher: each folder with its status; the default one first and selected', () async {
      final (t, _) = await twoFolders();
      final app = t.controller;
      expect(app.several, isTrue);
      expect(app.connections.map((c) => c.name), ['default', 'Work']);
      expect(app.switcher.map((c) => (c.name, c.label, c.icon, c.selected)), [
        ('default', 'CloudDrop', SummaryIcon.idle, true),
        ('Work', 'Work', SummaryIcon.syncing, false),
      ]);
      expect(app.switcher.first.line, 'Up to date');
      expect(app.switcher.last.line, startsWith('Uploading 2 files'));
      expect(app.rootName, 'CloudDrop');
      expect(windowTitle(app), 'CloudDrop', reason: 'not "CloudDrop - CloudDrop"');
      expect(app.snapshot.counts.synced, 1, reason: "the default folder's own database");
    });

    test('the folder remembered in app.json is shown (its name ignoring case)', () async {
      final (t, _) = await twoFolders(remember: 'work');
      final app = t.controller;
      expect(app.connectionName, 'Work');
      expect(app.label, 'Work');
      expect(app.rootName, 'Work');
      expect(windowTitle(app), 'Work - CloudDrop');
      expect(app.syncDir, '/home/me/Work');
      expect((app.snapshot.counts.synced, app.snapshot.counts.failed), (1, 1));
      expect(app.switcher.map((c) => c.selected), [false, true]);
      expect(app.switcher.first.icon, SummaryIcon.idle);
    });

    test('a remembered folder that is gone: the first one', () async {
      final (t, _) = await twoFolders(remember: 'Old Photos');
      expect(t.controller.connectionName, 'default');
    });

    test('without the default connection: the first of the others', () async {
      final t = TestApp();
      t.addConnection('Work');
      t.addConnection('Archive');
      t.engine.connectionList = [
        for (final c in t.engine.listed)
          if (!c.isDefault) c,
      ];
      final app = await t.ready();
      expect(app.connectionName, 'Work');
      await app.setPaused(true);
      expect(t.engine.log, ['Work: pause']);
    });

    test(
      'selecting a folder: its engine, database and status, and app.json remembers it',
      () async {
        final (t, work) = await twoFolders();
        final app = t.controller;
        var heard = 0;
        app.addListener(() => heard++);
        final version = app.connectionVersion.value;
        await app.selectConnection('Work');
        expect(heard, greaterThan(0));
        expect(app.connectionVersion.value, greaterThan(version));
        expect(app.connectionName, 'Work');
        expect(t.prefs.remembered, 'Work');
        expect(app.snapshot.daemon!.phase, Phase.uploading);
        expect(app.snapshot.counts.failed, 1);
        expect(app.config!.syncDir, '/home/me/Work');
        // Every action goes to Work, with its own folder's paths.
        await app.setPaused(true);
        await app.free(['contract.pdf']);
        await app.retry(['']);
        expect(work.engine.calls, ['pause', 'free /home/me/Work/contract.pdf', 'retry']);
        expect(t.engine.calls, isEmpty);
        expect(t.engine.log, [
          'Work: pause',
          'Work: free /home/me/Work/contract.pdf',
          'Work: retry',
        ]);
        // The service is the whole installation's.
        await app.startService();
        expect(t.engine.calls, ['service start']);
        // And back.
        await app.selectConnection('default');
        expect(app.snapshot.counts.failed, 0);
        expect(t.prefs.remembered, 'default');
        await app.setPaused(false);
        expect(t.engine.calls.last, 'resume');
      },
    );

    test('the other folder shows once it is read: no half-read screen in between', () async {
      final (t, _) = await twoFolders();
      final app = t.controller;
      final seen = <(String, bool, Phase?, int)>[];
      app.addListener(
        () => seen.add((
          app.connectionName,
          app.snapshot.cfg != null,
          app.snapshot.daemon?.phase,
          app.snapshot.counts.synced,
        )),
      );
      await app.selectConnection('Work');
      expect(seen, isNotEmpty);
      expect(seen.toSet(), {('Work', true, Phase.uploading, 1)});
    });

    test('only the folder shown keeps its database open', () async {
      final db = TestDb.memory();
      final reader = ReleaseRecorder(db.reader());
      final t = TestApp(db: db, reader: reader, daemon: heartbeat(Phase.idle));
      t.addConnection('Work');
      final app = await t.ready();
      expect(reader.released, 0);
      await app.selectConnection('Work');
      expect(reader.released, 1);
    });

    test('Files and Activity start over in the folder selected', () async {
      final (t, _) = await twoFolders();
      final app = t.controller;
      t.db.dir('Docs');
      t.db.file('Docs/a.txt', size: 1);
      final files = FilesController(app)..visible = true;
      addTearDown(files.dispose);
      final activity = ActivityController(app);
      addTearDown(activity.dispose);
      await activity.refresh();
      expect(activity.items.single.path, 'home.txt');
      files.openDir('Docs');
      await files.reload();
      files.selectOnly('Docs/a.txt');
      expect(files.dir, 'Docs');

      await app.selectConnection('Work');
      await files.reload();
      await Future<void>.delayed(Duration.zero);
      await activity.refresh();
      expect(files.dir, '');
      expect(files.selectedPaths, isEmpty);
      expect(files.items.map((i) => i.name), ['Plans', 'broken.bin', 'contract.pdf']);
      expect(activity.items.single.path, 'contract.pdf');
      expect(files.emptyState.title, 'Work Is Empty');
    });

    test("another folder's dot follows its status.json and its engine", () async {
      final (t, work) = await twoFolders();
      final app = t.controller;
      ConnectionEntry w() => app.switcher.last;

      work.status.value = heartbeat(Phase.paused);
      await app.poll();
      expect((w().icon, w().line), (SummaryIcon.paused, 'Paused'));

      work.status.value = heartbeat(Phase.offline, waiting: 1);
      await app.poll();
      expect(w().icon, SummaryIcon.attention);

      work.status.value = heartbeat(Phase.idle);
      await app.poll();
      expect((w().icon, w().line), (SummaryIcon.idle, 'Up to date'));

      // Failed uploads come with its `status --json`.
      work.engine.statusValue = EngineStatus(
        configured: true,
        signedIn: true,
        folder: '/home/me/Work',
        files: const {'error': 2},
      );
      await app.poll(forceEngine: true);
      expect((w().icon, w().line), (SummaryIcon.attention, '2 files failed'));

      // Signed out: the daemon runs no engine for it.
      work.engine.statusValue = EngineStatus(configured: true, folder: '/home/me/Work');
      work.status.value = null;
      await app.poll(forceEngine: true);
      expect((w().icon, w().line), (SummaryIcon.attention, 'Sign-in needed'));

      // Not running at all.
      work.engine.statusValue = EngineStatus(configured: true, signedIn: true, folder: '/x');
      await app.poll(forceEngine: true);
      expect((w().icon, w().line), (SummaryIcon.attention, 'Not running'));
    });

    test('a folder removed elsewhere: the app moves on to another', () async {
      final (t, _) = await twoFolders(remember: 'Work');
      final app = t.controller;
      expect(app.connectionName, 'Work');
      t.engine.connectionList = [t.engine.listed.first];
      await app.poll(forceEngine: true);
      expect(app.connectionName, 'default');
      expect(app.several, isFalse);
      expect(app.switcher, isEmpty);
      expect(app.snapshot.counts.synced, 1);
    });

    test('a folder added elsewhere appears in the switcher', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      final app = await t.ready();
      expect(app.switcher, isEmpty);
      t.addConnection('Photos', daemon: heartbeat(Phase.idle));
      await app.poll(forceEngine: true);
      expect(app.switcher.map((c) => c.name), ['default', 'Photos']);
      expect(app.connectionName, 'default');
    });

    test('signing out of one of several folders leaves Home offering the sign-in', () async {
      final (t, work) = await twoFolders(remember: 'Work');
      final app = t.controller;
      work.engine.result = (call) => const ActionResult(true, 'Signed out.');
      final r = await app.signOut();
      expect(r.ok, isTrue);
      expect(work.engine.calls, ['logout']);
      expect(t.engine.calls, isEmpty);
      expect(app.setupActive, isFalse, reason: 'the other folders stay a click away');
    });

    test("the settings file of the folder shown is the one named on its error page", () async {
      final (t, _) = await twoFolders(remember: 'Work');
      final app = t.controller;
      expect(app.configPath, '/home/me/.config/clouddrop/connections/Work/config.toml');
      app.openSettingsFolder();
      expect(t.system.opened, ['/home/me/.config/clouddrop/connections/Work']);
      await app.selectConnection('default');
      expect(app.configPath, '/home/me/.config/clouddrop/config.toml');
    });

    test('with several folders, one that needs a sign-in says so on Home, no assistant', () async {
      final t = TestApp(status: const EngineStatus(configured: true, signedIn: false));
      t.addConnection('Work');
      final app = await t.ready();
      expect(app.setupActive, isFalse);
      expect(summarize(app.snapshot).action, SummaryAction.signIn);
    });
  });

  group('Remove This Folder', () {
    test('removes the folder shown, then shows another and remembers it', () async {
      final (t, _) = await twoFolders(remember: 'Work');
      final app = t.controller;
      final messages = <String>[];
      app.messages.listen((m) => messages.add(m.text));
      final r = await app.removeConnection();
      expect(r.ok, isTrue);
      expect(t.engine.log, ['default: connections remove Work']);
      expect(app.connections.map((c) => c.name), ['default']);
      expect(app.connectionName, 'default');
      expect(app.several, isFalse);
      expect(t.prefs.remembered, 'default');
      expect(app.setupActive, isFalse);
      expect(app.snapshot.counts.synced, 1);
      await Future<void>.delayed(Duration.zero);
      expect(messages.last, 'CloudDrop no longer uploads “Work”. Nothing was deleted.');
    });

    test('removing the default connection keeps the others', () async {
      final (t, _) = await twoFolders();
      final app = t.controller;
      await app.removeConnection();
      expect(t.engine.log, ['default: connections remove default']);
      expect(app.connectionName, 'Work');
      expect(app.connections.map((c) => c.name), ['Work']);
      expect(t.prefs.remembered, 'Work');
      expect(app.syncDir, '/home/me/Work');
    });

    test('the last folder: the setup assistant is next', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      final app = await t.ready();
      final r = await app.removeConnection();
      expect(r.ok, isTrue);
      expect(app.connections, isEmpty);
      expect(app.connectionName, 'default');
      expect(app.setupActive, isTrue);
      expect(app.setupAdding, isFalse, reason: 'the first-run assistant');
      expect(app.snapshot.cfg, isNull);
    });

    test(
      'the database is let go before the engine deletes it (Windows keeps open files)',
      () async {
        final db = TestDb.memory();
        final reader = ReleaseRecorder(db.reader());
        final t = TestApp(db: db, reader: reader, daemon: heartbeat(Phase.idle));
        t.addConnection('Work');
        await t.ready();
        var releasedFirst = false;
        t.engine.result = (call) {
          if (call.startsWith('connections remove')) releasedFirst = reader.released > 0;
          return const ActionResult(true, 'Removed.');
        };
        await t.controller.removeConnection();
        expect(releasedFirst, isTrue);
      },
    );

    test('a failure says why and changes nothing', () async {
      final (t, _) = await twoFolders(remember: 'Work');
      final app = t.controller;
      t.engine.result = (call) => call.startsWith('connections remove')
          ? const ActionResult(false, 'CloudDrop is still uploading to "Work"')
          : const ActionResult(true, 'ok');
      final messages = <AppMessage>[];
      app.messages.listen(messages.add);
      final r = await app.removeConnection();
      expect(r.ok, isFalse);
      expect(app.connectionName, 'Work');
      expect(app.several, isTrue);
      await Future<void>.delayed(Duration.zero);
      expect(messages.single.severity, Severity.error);
      expect(
        messages.single.text,
        'Couldn\'t remove “Work”: CloudDrop is still uploading to "Work"',
      );
    });
  });

  group('Add Another Folder', () {
    Future<SetupController> adding(TestApp t) async {
      t.controller.beginAddConnection();
      expect(t.controller.setupAdding, isTrue);
      final c = SetupController(t.controller, adding: true);
      addTearDown(c.dispose);
      return c;
    }

    /// Provider (as chosen) -> folder [dir] -> the sign-in page.
    Future<void> toSignIn(TestApp t, SetupController c, String dir) async {
      c.next();
      expect(c.step, SetupStep.folder);
      t.system.directory = dir;
      await c.chooseFolder();
      c.next();
      expect(c.step, SetupStep.signIn);
    }

    test('no welcome, a way out, nothing taken from the folder shown now', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      expect(c.title, 'Add Another Folder');
      expect(c.steps, [SetupStep.provider, SetupStep.folder, SetupStep.signIn, SetupStep.done]);
      expect(c.canCancel, isTrue);
      expect(c.chosen, CloudProvider.google);
      expect(c.folder, '', reason: 'a folder of its own is chosen');
      expect(c.alreadySignedIn, isFalse);
      c.next();
      expect(c.folderValid, isFalse);
    });

    test('the Google key page when no Google folder has a key and the build has none', () async {
      final t = TestApp(provider: webdav, daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      expect(c.steps.contains(SetupStep.key), isTrue);
      c.choose(oneDrive);
      expect(c.steps.contains(SetupStep.key), isFalse);
    });

    test(
      'the folder names the connection and its cloud folder; taken names get a number',
      () async {
        final t = TestApp(daemon: heartbeat(Phase.idle));
        t.addConnection('Work');
        await t.ready();
        final c = await adding(t);
        c.next();
        t.system.directory = '/media/usb/work';
        await c.chooseFolder();
        expect(c.folder, '/media/usb/work');
        expect(c.newConnectionName, 'work 2');
        expect(c.driveFolder, 'work');
        expect(c.folderValid, isTrue);
        // A cloud folder name the user typed stays.
        c.setDriveFolder('Backups');
        t.system.directory = '/media/usb/Photos';
        await c.chooseFolder();
        expect((c.newConnectionName, c.driveFolder), ('Photos', 'Backups'));
      },
    );

    test('a folder that already uploads is refused before the engine is asked', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      t.addConnection('Work');
      await t.ready();
      final c = await adding(t);
      c.next();
      for (final (dir, words) in [
        ('/home/me/CloudDrop', 'already uploads as “CloudDrop”'),
        ('/home/me/CloudDrop/Photos', 'inside “CloudDrop”'),
        ('/home/me', 'holds “CloudDrop”'),
        ('/home/me/Work/Sub', 'inside “Work”'),
      ]) {
        t.system.directory = dir;
        await c.chooseFolder();
        expect(c.folderProblem, contains(words), reason: dir);
        expect(c.folderValid, isFalse);
      }
      t.system.directory = '/home/me/Music';
      await c.chooseFolder();
      expect(c.folderProblem, isNull);
      expect(c.folderValid, isTrue);
    });

    test('sign in: connections add, then the new connection signs in; Finish shows it', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      await toSignIn(t, c, '/home/me/Work');
      final f = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.log, [
        'default: connections add Work folder=/home/me/Work drive=Work provider=gdrive',
      ]);
      expect(c.createdName, 'Work');
      final login = t.engine.view('Work').logins.single;
      expect(t.engine.logins, isEmpty, reason: 'the new connection signs in, not the default one');
      login.urlC.complete('https://accounts.example/auth');
      login.doneC.complete(const ActionResult(true, 'Signed in.'));
      await f;
      expect(c.step, SetupStep.done);
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(c.serviceState, ServiceStart.running);
      expect(
        t.engine.calls.where((x) => x.startsWith('service')),
        isEmpty,
        reason: 'the running service takes the folder on; "start at sign-in" is left alone',
      );
      final app = t.controller;
      final seen = <(String, bool)>[];
      app.addListener(() => seen.add((app.connectionName, app.setupActive)));
      await c.finish();
      expect(seen.toSet(), {('Work', false)}, reason: 'the main window opens on the new folder');
      expect(app.setupActive, isFalse);
      expect(app.connectionName, 'Work');
      expect(t.prefs.remembered, 'Work');
      expect(app.switcher.map((e) => e.name), ['default', 'Work']);
    });

    test('the service is started if it is not running', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      t.engine.service = const ServiceStatus(running: false, startsAtSignIn: false);
      await t.ready();
      final c = await adding(t);
      await c.startService();
      expect(t.engine.calls, ['service start']);
      expect(c.serviceState, ServiceStart.running);
    });

    test("a folder the engine refuses shows its sentence and nothing is created", () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      t.engine.result = (call) => call.startsWith('connections add')
          ? const ActionResult(
              false,
              '/srv/x holds the folder of the connection "default" (/home/me/CloudDrop); each '
              'connection needs a folder of its own',
            )
          : const ActionResult(true, 'ok');
      final c = await adding(t);
      await toSignIn(t, c, '/srv/x');
      await c.signInWithBrowser();
      expect(c.setupError, endsWith('each connection needs a folder of its own'));
      expect(c.createdName, isNull);
      expect(t.engine.views['x']?.logins ?? const [], isEmpty);
      await c.cancel();
      expect(t.engine.log.where((x) => x.contains('remove')), isEmpty);
      expect(t.controller.setupActive, isFalse);
    });

    test('Cancel after the folder was created removes it again', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      await toSignIn(t, c, '/home/me/Work');
      final f = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(c.canCancel, isTrue);
      await c.cancel();
      await f;
      final login = t.engine.view('Work').logins.single;
      expect(login.cancelled, isTrue, reason: 'the browser sign-in stops');
      expect(t.engine.log.last, 'default: connections remove Work');
      expect(t.engine.listed.map((x) => x.name), ['default']);
      expect(t.controller.setupActive, isFalse);
      expect(t.controller.connectionName, 'default');
    });

    test('no Cancel while the folder is being written', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      t.engine.providerList = allProviders;
      await t.ready();
      final c = await adding(t);
      c.choose(webdav);
      await toSignIn(t, c, '/srv/nas');
      c.setField('url', 'https://nas/dav');
      c.setField('user', 'me');
      c.setField('password', 'pw');
      final testing = c.testConnection();
      expect(c.test, ConnectionTest.testing);
      expect(c.canCancel, isFalse);
      await testing;
      expect(c.canCancel, isTrue);
    });

    test('Cancel before anything was written removes nothing', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      c.next();
      await c.cancel();
      expect(t.engine.log, isEmpty);
      expect(t.controller.setupActive, isFalse);
    });

    test('a sign-in tried again updates the new connection with setup --connection', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      await toSignIn(t, c, '/home/me/Work');
      var f = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      t.engine.view('Work').logins.single.urlC.completeError(StateError('denied'));
      await f;
      expect(c.signIn.phase, SignInPhase.failed);
      f = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.log.where((x) => x.contains('connections add')), hasLength(1));
      expect(t.engine.view('Work').calls, [
        'login',
        'setup folder=/home/me/Work drive=Work json=null id=null secret=null provider=gdrive',
        'login',
      ]);
      t.engine.view('Work').logins.last
        ..urlC.complete('https://accounts.example/auth')
        ..doneC.complete(const ActionResult(true, 'Signed in.'));
      await f;
      expect(c.step, SetupStep.done);
    });

    test('another folder chosen after it was created: it starts over under the new name', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      await t.ready();
      final c = await adding(t);
      await toSignIn(t, c, '/home/me/Work');
      final f = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      c.back(); // the sign-in stops; back to the folders
      await f;
      t.system.directory = '/home/me/Photos';
      await c.chooseFolder();
      c.next();
      final g = c.signInWithBrowser();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.log, [
        'default: connections add Work folder=/home/me/Work drive=Work provider=gdrive',
        'default: connections remove Work',
        'default: connections add Photos folder=/home/me/Photos drive=Photos provider=gdrive',
      ]);
      expect(c.createdName, 'Photos');
      t.engine.view('Photos').logins.single
        ..urlC.complete('https://accounts.example/auth')
        ..doneC.complete(const ActionResult(true, 'Signed in.'));
      await g;
      expect(c.step, SetupStep.done);
    });

    test('a form: Test Connection adds it with its settings, then checks the new one', () async {
      final t = TestApp(daemon: heartbeat(Phase.idle));
      t.engine.providerList = allProviders;
      await t.ready();
      final c = await adding(t);
      c.choose(webdav);
      await toSignIn(t, c, '/srv/nas');
      c.setField('url', 'https://nas/dav');
      c.setField('user', 'me');
      c.setField('password', 'pw');
      t.engine.view('nas').result = (call) => call == 'check'
          ? const ActionResult(true, 'Connected to nas as me.')
          : const ActionResult(true, 'ok');
      await c.testConnection();
      expect(t.engine.log, [
        'default: connections add nas folder=/srv/nas drive=nas provider=webdav '
            '--set url=https://nas/dav --set user=me --secret password=pw',
        'nas: check',
      ]);
      expect(c.test, ConnectionTest.ok);
      await c.continueConnected();
      expect(c.step, SetupStep.done);
      expect(t.engine.log.where((x) => x.contains('reset-cloud')), isEmpty);
    });
  });
}
