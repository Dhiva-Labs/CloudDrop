import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/activity_text.dart';
import 'package:clouddrop_app/logic/fmt.dart' as fmt;
import 'package:clouddrop_app/state/activity_controller.dart';
import 'package:clouddrop_app/state/setup_controller.dart';
import 'dart:async';

import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';

void main() {
  group('ActivityController', () {
    test('loads newest first, only when something new was logged', () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      for (var i = 1; i <= 150; i++) {
        t.db.activity('uploaded', 'f$i.txt', at: i * 1000, bytes: i);
      }
      final app = await t.ready();
      final c = ActivityController(app)..visible = true;
      addTearDown(c.dispose);
      await c.refresh();
      expect(c.loaded, isTrue);
      expect(c.items.first.path, 'f150.txt');
      expect(c.visibleItems.length, 100);
      expect(c.hasMore, isTrue);
      c.showMore();
      expect(c.visibleItems.length, 150 > 200 ? 200 : 150);
      expect(c.hasMore, isFalse);

      var n = 0;
      c.addListener(() => n++);
      await c.refresh();
      expect(n, 0, reason: 'nothing new: no reload');
      t.db.activity('failed', 'z.bin', at: 999999, detail: 'boom');
      await c.refresh();
      expect(n, 1);
      expect(c.items.first.path, 'z.bin');
    });

    test("the filter narrows the list; Home's recent rows ignore it", () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      for (var i = 1; i <= 120; i++) {
        t.db.activity(i % 4 == 0 ? 'failed' : 'uploaded', 'f$i.txt', at: i * 1000, detail: 'x');
      }
      final app = await t.ready();
      final c = ActivityController(app)..visible = true;
      addTearDown(c.dispose);
      await c.refresh();
      expect(c.recent.map((a) => a.path), ['f120.txt', 'f119.txt', 'f118.txt', 'f117.txt']);
      c.filter = ActivityFilter.problems;
      expect(c.visibleItems.length, 30);
      expect(c.visibleItems.every((a) => a.kind == ActivityKind.failed), isTrue);
      expect(c.hasMore, isFalse);
      c.filter = ActivityFilter.downloads;
      expect(c.visibleItems, isEmpty);
      expect(c.recent.length, ActivityController.recentCount);
    });

    test('loads while Home or Activity shows', () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      t.db.activity('uploaded', 'a.txt', at: 1000);
      final app = await t.ready();
      final c = ActivityController(app);
      addTearDown(c.dispose);
      await Future<void>.delayed(Duration.zero);
      expect(c.loaded, isTrue, reason: 'the app opens on Home');
    });

    test('empty history', () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      final app = await t.ready();
      final c = ActivityController(app);
      addTearDown(c.dispose);
      await c.refresh();
      expect(c.loaded, isTrue);
      expect(c.items, isEmpty);
    });

    test('the day changing reloads (headings say Today / Yesterday)', () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      t.db.activity('uploaded', 'a', at: 1000);
      final app = await t.ready();
      final c = ActivityController(app);
      addTearDown(c.dispose);
      fmt.nowProvider = () => DateTime.utc(2026, 10, 14, 10);
      addTearDown(() => fmt.nowProvider = DateTime.now);
      await c.refresh();
      var n = 0;
      c.addListener(() => n++);
      fmt.nowProvider = () => DateTime.utc(2026, 10, 15, 0, 1);
      await c.refresh();
      expect(n, 1);
    });
  });

  group('parseClientJson (control::parse_client_json)', () {
    test('installed, web and bare shapes', () {
      const installed =
          '{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p","client_secret":"test-secret","redirect_uris":["http://localhost"]}}';
      final k = parseClientJson(installed);
      expect((k.id, k.secret), ('123-abc.apps.googleusercontent.com', 'test-secret'));
      expect(
        parseClientJson(
          '{"web":{"client_id":"w.apps.googleusercontent.com","client_secret":"s"}}',
        ).id,
        'w.apps.googleusercontent.com',
      );
      expect(
        parseClientJson(
          '{"client_id":" 1.apps.googleusercontent.com ","client_secret":" s "}',
        ).secret,
        's',
      );
    });

    test('rejects files that are not a client key', () {
      expect(() => parseClientJson('{"installed":{}}'), throwsFormatException);
      expect(() => parseClientJson('not json'), throwsFormatException);
      expect(() => parseClientJson('[1,2]'), throwsFormatException);
    });

    test('client id shape', () {
      expect(looksLikeClientId('12-ab.apps.googleusercontent.com'), isTrue);
      expect(looksLikeClientId(' 12-ab.apps.googleusercontent.com '), isTrue);
      expect(looksLikeClientId('.apps.googleusercontent.com'), isFalse);
      expect(looksLikeClientId('not-a-client-id'), isFalse);
    });

    test('the six Google steps open the same pages as the GTK assistant', () {
      expect(googleKeySteps.map((s) => s.url), [
        'https://console.cloud.google.com/projectcreate',
        'https://console.cloud.google.com/apis/library/drive.googleapis.com',
        'https://console.cloud.google.com/auth/overview',
        'https://console.cloud.google.com/auth/scopes',
        'https://console.cloud.google.com/auth/audience',
        'https://console.cloud.google.com/auth/clients/create',
      ]);
    });
  });

  group('SetupController', () {
    Future<(TestApp, SetupController)> make({
      bool builtin = false,
      bool signedIn = false,
      String? locked,
    }) async {
      final t = TestApp(
        status: EngineStatus(configured: false, hasBuiltinClient: builtin, signedIn: signedIn),
      );
      t.engine.config = null;
      final app = await t.ready();
      final c = SetupController(app, lockedRoot: locked);
      addTearDown(c.dispose);
      return (t, c);
    }

    test('the Google key page only appears without a built-in client', () async {
      var (t, c) = await make(builtin: false);
      expect(c.steps, [
        SetupStep.welcome,
        SetupStep.key,
        SetupStep.folder,
        SetupStep.signIn,
        SetupStep.done,
      ]);
      (t, c) = await make(builtin: true);
      expect(c.steps, [SetupStep.welcome, SetupStep.folder, SetupStep.signIn, SetupStep.done]);
      expect(c.stepCount, 4);
      expect(c.step, SetupStep.welcome);
      expect(c.canGoBack, isFalse);
      c.next();
      expect(c.step, SetupStep.folder);
      expect(c.canGoBack, isTrue);
      c.back();
      expect(c.step, SetupStep.welcome);
    });

    test('after signing out the assistant starts on the sign-in page', () async {
      final t = TestApp(status: const EngineStatus(configured: true, signedIn: false));
      final app = await t.ready();
      final c = SetupController(app, startAtSignIn: true);
      addTearDown(c.dispose);
      expect(c.step, SetupStep.signIn);
      expect(c.canGoBack, isTrue);
      expect(c.alreadySignedIn, isFalse, reason: 'so "Keep Current Sign-in" is not offered');
      expect(c.folder, '/home/me/CloudDrop');
      expect(c.hasStoredSecret, isTrue, reason: 'the stored Google key is reused');
      // Signing in again re-sends nothing: the engine keeps its key and folder.
      unawaited(c.signInWithGoogle());
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(
        t.engine.calls.first,
        'setup folder=/home/me/CloudDrop drive=CloudDrop json=null id=null secret=null',
      );
      expect(t.engine.calls.last, 'login');
      c.signIn.cancel();
      c.back();
      expect(c.step, SetupStep.folder);
    });

    test('defaults: %USERPROFILE%\\CloudDrop, folder name CloudDrop', () async {
      final (_, c) = await make();
      expect(c.folder, '/home/me/CloudDrop');
      expect(c.driveFolder, 'CloudDrop');
      expect(c.folderLocked, isFalse);
      final (_, c2) = await make(locked: '/data/Locked');
      expect(c2.folder, '/data/Locked');
      expect(c2.folderLocked, isTrue);
    });

    test(
      'key: valid only with a proper id and a secret; a JSON pasted in the id field fills both',
      () async {
        final (_, c) = await make();
        expect(c.keyValid, isFalse);
        c.setClientId('123-abc.apps.googleusercontent.com');
        expect(c.keyValid, isFalse);
        c.setClientSecret('GOCSPX-x');
        expect(c.keyValid, isTrue);
        c.setClientId('nonsense');
        expect(c.keyValid, isFalse);
        c.setClientId(
          '{"installed":{"client_id":"9-z.apps.googleusercontent.com","client_secret":"S"}}',
        );
        expect((c.clientId, c.clientSecret), ('9-z.apps.googleusercontent.com', 'S'));
        expect(c.keyError, isNull);
        expect(c.keyValid, isTrue);
        c.setClientId('{ broken');
        expect(c.keyError, 'This is not a Google client JSON file');
      },
    );

    test('choosing the downloaded file fills the fields and remembers the file', () async {
      final (t, c) = await make();
      t.system.jsonFile = '/home/me/Downloads/client_secret_1.json';
      c.readFile = (p) async =>
          '{"installed":{"client_id":"7-q.apps.googleusercontent.com","client_secret":"SS"}}';
      await c.chooseKeyFile();
      expect(c.clientId, '7-q.apps.googleusercontent.com');
      expect(c.keyFile, '/home/me/Downloads/client_secret_1.json');
      c.setClientSecret('edited');
      expect(c.keyFile, isNull, reason: 'edited by hand: the temp-file route is used instead');

      t.system.jsonFile = '/home/me/Downloads/other.json';
      c.readFile = (p) async => '{"installed":{}}';
      await c.chooseKeyFile();
      expect(c.keyError, contains('no client_id/client_secret'));

      t.system.jsonFile = null;
      await c.chooseKeyFile(); // cancelled: nothing changes
    });

    test('changing the folder, unless it is locked', () async {
      final (t, c) = await make();
      t.system.directory = '/data/Mine';
      await c.chooseFolder();
      expect(c.folder, '/data/Mine');
      final (t2, c2) = await make(locked: '/data/Locked');
      t2.system.directory = '/elsewhere';
      await c2.chooseFolder();
      expect(c2.folder, '/data/Locked');
      c.setDriveFolder('  ');
      expect(c.folderValid, isFalse);
      c.setDriveFolder('Backups');
      expect(c.folderValid, isTrue);
    });

    test('sign in: setup first, then the browser; moves on when signed in', () async {
      final (t, c) = await make(builtin: true);
      c.next(); // folder
      c.setDriveFolder(' My Drive Backup ');
      c.next(); // sign-in
      expect(c.step, SetupStep.signIn);
      final f = c.signInWithGoogle();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(
        t.engine.calls.first,
        'setup folder=/home/me/CloudDrop drive=My Drive Backup json=null id=null secret=null',
      );
      expect(t.engine.calls.last, 'login');
      t.engine.logins.single.urlC.complete('https://accounts.example/auth?x=1');
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(c.signIn.phase, SignInPhase.waiting);
      expect(c.signIn.url, 'https://accounts.example/auth?x=1');
      expect(t.system.urls, ['https://accounts.example/auth?x=1']);
      t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
      await f;
      expect(c.step, SetupStep.done);
    });

    test('with your own key the typed id/secret go to setup', () async {
      final (t, c) = await make();
      c.setClientId('123-abc.apps.googleusercontent.com');
      c.setClientSecret('sec');
      unawaited(c.signInWithGoogle());
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.calls.first, contains('id=123-abc.apps.googleusercontent.com secret=sec'));
    });

    test('a chosen key file is passed as --client-json', () async {
      final (t, c) = await make();
      c.keyFile = '/dl/k.json';
      c.clientId = '1.apps.googleusercontent.com';
      c.clientSecret = 's';
      unawaited(c.signInWithGoogle());
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.calls.first, contains('json=/dl/k.json'));
    });

    test('setup errors stop the sign-in and are shown', () async {
      final (t, c) = await make(builtin: true);
      t.engine.result = (call) => call.startsWith('setup')
          ? const ActionResult(
              false,
              'CloudDrop already uses /x; the folder can\'t be changed after setup',
            )
          : const ActionResult(true, '');
      await c.signInWithGoogle();
      expect(c.setupError, contains("can't be changed after setup"));
      expect(t.engine.calls, hasLength(1));
      expect(c.signIn.phase, SignInPhase.idle);
    });

    test('a failed or cancelled sign-in can be retried', () async {
      final (t, c) = await make(builtin: true);
      c.next();
      c.next();
      var f = c.signInWithGoogle();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      t.engine.logins.last.urlC.complete('https://a.example/auth');
      t.engine.logins.last.doneC.complete(const ActionResult(false, 'access_denied'));
      await f;
      expect(c.signIn.phase, SignInPhase.failed);
      expect(c.signIn.error, 'access_denied');
      expect(c.step, SetupStep.signIn);

      f = c.signInWithGoogle();
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(c.signIn.phase, anyOf(SignInPhase.starting, SignInPhase.waiting));
      c.signIn.cancel();
      expect(t.engine.logins.last.cancelled, isTrue);
      expect(c.signIn.phase, SignInPhase.idle);
      await f;
      expect(c.step, SetupStep.signIn);
    });

    test('keeping the current sign-in only writes the config', () async {
      final (t, c) = await make(builtin: true, signedIn: true);
      c.next();
      c.next();
      expect(c.alreadySignedIn, isTrue);
      await c.keepCurrentSignIn();
      expect(t.engine.calls.first, startsWith('setup '));
      expect(t.engine.calls.where((c) => c == 'login'), isEmpty);
      expect(c.step, SetupStep.done);
    });

    test('an existing key is kept; editing the id asks for a new secret', () async {
      final t = TestApp(status: const EngineStatus(configured: true, signedIn: false));
      final app = await t.ready();
      final c = SetupController(app);
      addTearDown(c.dispose);
      expect(c.clientId, '123-test.apps.googleusercontent.com');
      expect(c.hasStoredSecret, isTrue);
      expect(c.keyValid, isTrue, reason: 'the stored secret still goes with the stored id');
      unawaited(c.signInWithGoogle());
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.calls.first, contains('id=null secret=null'), reason: 'nothing is re-sent');
      c.signIn.cancel();

      c.setClientId('999-other.apps.googleusercontent.com');
      expect(c.hasStoredSecret, isFalse);
      expect(c.keyValid, isFalse, reason: 'a different id needs its own secret');
      c.setClientSecret('new-secret');
      expect(c.keyValid, isTrue);
    });

    test('the last page starts the service; failures offer a retry', () async {
      final (t, c) = await make(builtin: true);
      for (var i = 0; i < 3; i++) {
        c.next();
      }
      expect(c.step, SetupStep.done);
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(t.engine.calls, ['service enable']);
      expect(c.serviceState, ServiceStart.running);

      t.engine.result = (call) => const ActionResult(false, 'could not register');
      t.engine.service = const ServiceStatus(running: false, startsAtSignIn: false);
      await c.startService();
      expect(c.serviceState, ServiceStart.failed);
      expect(c.serviceError, 'could not register');

      // Registering failed but it is running anyway.
      t.engine.service = const ServiceStatus(running: true, startsAtSignIn: false);
      await c.startService();
      expect(c.serviceState, ServiceStart.running);
      expect(c.serviceError, contains('may not start by itself'));
    });

    test('open folder and finish', () async {
      final (t, c) = await make(builtin: true);
      c.openFolder();
      expect(t.system.opened, ['/home/me/CloudDrop']);
      t.engine.config = const AppConfig(
        syncDir: '/home/me/CloudDrop',
        driveFolder: 'CloudDrop',
        clientId: 'a.apps.googleusercontent.com',
        hasClientSecret: true,
      );
      t.engine.statusValue = const EngineStatus(
        configured: true,
        signedIn: true,
        folder: '/home/me/CloudDrop',
      );
      await c.finish();
      expect(c.app.setupActive, isFalse);
    });
  });
}
