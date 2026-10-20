/// The first-run assistant, clicked through page by page.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/harness.dart';
import '../support/scenarios.dart';
import '../support/test_db.dart';

TestApp fresh({bool builtin = false, bool signedIn = false, TestDb? db}) {
  final t = TestApp(
    db: db,
    windows: true,
    status: EngineStatus(configured: false, hasBuiltinClient: builtin, signedIn: signedIn),
  );
  t.engine.config = null;
  return t;
}

Future<void> tap(WidgetTester tester, String text, {int index = 0}) async {
  final f = find.text(text).at(index);
  await tester.ensureVisible(f);
  await tester.tap(f);
  await tester.settle();
}

Finder button(String text) => find.widgetWithText(Button, text);
Finder filled(String text) => find.widgetWithText(FilledButton, text);

bool enabled(WidgetTester tester, Finder f) {
  final w = tester.widget(f);
  if (w is FilledButton) return w.onPressed != null;
  if (w is Button) return w.onPressed != null;
  throw ArgumentError('not a button: $w');
}

/// What the engine looks like once setup + sign-in succeeded.
void signedInEngine(TestApp t) {
  t.engine.config = busyConfig;
  t.engine.statusValue = EngineStatus(
    configured: true,
    signedIn: true,
    hasBuiltinClient: false,
    folder: t.syncDir,
    driveFolder: 'CloudDrop',
  );
}

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  uiTest('without a built-in client: welcome, key, folders, sign in, done, finish', (tester) async {
    String? clip;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (
      call,
    ) async {
      if (call.method == 'Clipboard.setData') clip = (call.arguments as Map)['text'] as String?;
      return null;
    });
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );

    final t = fresh();
    await pumpApp(tester, t, size: const Size(1000, 1300));

    // 1. Welcome
    expect(find.text('Welcome to CloudDrop'), findsOneWidget);
    expect(find.text('Step 1 of 5 · Welcome'), findsOneWidget);
    expect(find.text('Keeps your folders'), findsOneWidget);
    await tap(tester, 'Get Started');

    // 2. Google key: the six guided steps, each with an Open button
    expect(find.text('Connect a Google key'), findsOneWidget);
    expect(find.text('Step 2 of 5 · Google key'), findsOneWidget);
    for (final title in [
      '1. Create a project',
      '2. Turn on the Google Drive API',
      '3. Set up the consent screen',
      "4. Allow access to CloudDrop's own files",
      '5. Publish the app',
      '6. Create a Desktop app client',
    ]) {
      expect(find.text(title), findsOneWidget, reason: title);
    }
    expect(find.text('Open'), findsNWidgets(6));
    await tester.tap(find.text('Open').first);
    await tester.tap(find.text('Open').last);
    await tester.pump();
    expect(t.system.urls, [
      'https://console.cloud.google.com/projectcreate',
      'https://console.cloud.google.com/auth/clients/create',
    ]);

    // Continue stays off until there is a real client id and a secret.
    expect(enabled(tester, filled('Continue')), isFalse);
    await tester.enterText(find.byType(TextBox).first, 'not-an-id');
    await tester.enterText(find.byType(PasswordBox), 's');
    await tester.settle();
    expect(enabled(tester, filled('Continue')), isFalse);
    await tester.enterText(find.byType(TextBox).first, '555-abc.apps.googleusercontent.com');
    await tester.settle();
    expect(enabled(tester, filled('Continue')), isTrue);
    await tap(tester, 'Continue');

    // 3. Folders: the default, and a Drive folder name
    expect(find.text('Choose your folders'), findsOneWidget);
    expect(find.text(r'C:\Users\Dhiva\CloudDrop'), findsOneWidget);
    t.system.directory = r'D:\Backups\CloudDrop';
    await tap(tester, 'Change…');
    expect(find.text(r'D:\Backups\CloudDrop'), findsOneWidget);
    await tester.enterText(find.byType(TextBox).last, '  My Backups ');
    await tester.settle();
    await tap(tester, 'Continue');

    // 4. Sign in
    expect(find.text('Sign in to Google'), findsOneWidget);
    expect(
      find.textContaining("isn't verified"),
      findsOneWidget,
      reason: 'own key: the warning about Google\'s notice',
    );
    expect(find.text('Keep Current Sign-in'), findsNothing);
    await tap(tester, 'Sign In with Google');
    expect(
      t.engine.calls.first,
      r'setup folder=D:\Backups\CloudDrop drive=My Backups json=null id=555-abc.apps.googleusercontent.com secret=s',
    );
    expect(t.engine.calls.last, 'login');
    expect(find.text('Waiting for you to finish in the browser…'), findsOneWidget);
    expect(find.text('Copy sign-in link'), findsOneWidget);
    t.engine.logins.single.urlC.complete('https://accounts.google.com/o/oauth2/v2/auth?state=abc');
    await tester.settle();
    expect(t.system.urls.last, 'https://accounts.google.com/o/oauth2/v2/auth?state=abc');
    await tap(tester, 'Copy sign-in link');
    expect(clip, 'https://accounts.google.com/o/oauth2/v2/auth?state=abc');
    signedInEngine(t);
    t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
    await tester.settle();

    // 5. Done: the service is enabled, Explorer is explained
    expect(find.text("You're all set"), findsOneWidget);
    expect(t.engine.calls.last, 'service enable');
    expect(find.text('CloudDrop is running'), findsOneWidget);
    expect(find.textContaining("File Explorer's sidebar"), findsOneWidget);
    expect(find.textContaining(r'Drop files into D:\Backups\CloudDrop'), findsOneWidget);
    expect(find.textContaining('My Drive → My Backups'), findsOneWidget);
    await tap(tester, 'Open CloudDrop Folder');
    expect(t.system.opened, [r'D:\Backups\CloudDrop']);
    await tap(tester, 'Finish');
    expect(find.text('Welcome to CloudDrop'), findsNothing);
    expect(find.text('Home'), findsWidgets);
    expect(t.controller.setupActive, isFalse);
  });

  uiTest('with a built-in client there is no key page', (tester) async {
    final t = fresh(builtin: true);
    await pumpApp(tester, t);
    expect(find.text('Step 1 of 4 · Welcome'), findsOneWidget);
    await tap(tester, 'Get Started');
    expect(find.text('Choose your folders'), findsOneWidget);
    expect(find.text('Connect a Google key'), findsNothing);
    expect(find.text('Step 2 of 4 · Folders'), findsOneWidget);
    await tap(tester, 'Continue');
    expect(find.textContaining("isn't verified"), findsNothing);
    await tap(tester, 'Sign In with Google');
    expect(
      t.engine.calls.first,
      r'setup folder=C:\Users\Dhiva\CloudDrop drive=CloudDrop json=null id=null secret=null',
    );
  });

  uiTest('pasting the whole key JSON into the id box fills both fields', (tester) async {
    final t = fresh();
    await pumpApp(tester, t, size: const Size(1000, 1300));
    await tap(tester, 'Get Started');
    await tester.enterText(
      find.byType(TextBox).first,
      '{"installed":{"client_id":"9-z.apps.googleusercontent.com","client_secret":"SEC"}}',
    );
    await tester.settle();
    expect(find.text('9-z.apps.googleusercontent.com'), findsOneWidget);
    expect(enabled(tester, filled('Continue')), isTrue);
    await tester.enterText(find.byType(TextBox).first, '{ broken');
    await tester.settle();
    expect(find.text('This is not a Google client JSON file'), findsOneWidget);
  });

  uiTest('Back goes to the previous page', (tester) async {
    final t = fresh(builtin: true);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    expect(find.text('Choose your folders'), findsOneWidget);
    await tester.tap(find.byTooltip('Back'));
    await tester.settle();
    expect(find.text('Welcome to CloudDrop'), findsOneWidget);
  });

  uiTest('a folder CloudDrop already used cannot be changed', (tester) async {
    final db = TestDb.memory()..meta('sync_root', r'E:\Old\CloudDrop');
    final t = fresh(builtin: true, db: db);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    expect(find.text(r'E:\Old\CloudDrop'), findsOneWidget);
    expect(enabled(tester, button('Change…')), isFalse);
  });

  uiTest('an empty Drive folder name blocks Continue', (tester) async {
    final t = fresh(builtin: true);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    await tester.enterText(find.byType(TextBox).last, '   ');
    await tester.settle();
    expect(enabled(tester, filled('Continue')), isFalse);
  });

  uiTest('a failed or cancelled sign-in leaves the button to try again', (tester) async {
    final t = fresh(builtin: true);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    await tap(tester, 'Continue');
    await tap(tester, 'Sign In with Google');
    t.engine.logins.last.urlC.complete('https://accounts.example/auth');
    t.engine.logins.last.doneC.complete(const ActionResult(false, 'Google said: access_denied'));
    await tester.settle();
    expect(find.text("Sign-in didn't finish: Google said: access_denied"), findsOneWidget);
    expect(find.text('Sign In with Google'), findsOneWidget);

    await tap(tester, 'Sign In with Google');
    expect(find.text('Waiting for you to finish in the browser…'), findsOneWidget);
    await tap(tester, 'Cancel');
    expect(t.engine.logins.last.cancelled, isTrue);
    expect(find.text('Sign In with Google'), findsOneWidget);
    expect(find.text('Waiting for you to finish in the browser…'), findsNothing);
  });

  uiTest('setup errors are shown on the sign-in page', (tester) async {
    final t = fresh(builtin: true);
    t.engine.result = (c) => c.startsWith('setup')
        ? const ActionResult(
            false,
            "CloudDrop already uses D:\\X; the folder can't be changed after setup",
          )
        : const ActionResult(true, '');
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    await tap(tester, 'Continue');
    await tap(tester, 'Sign In with Google');
    expect(find.textContaining("can't be changed after setup"), findsOneWidget);
    expect(t.engine.logins, isEmpty, reason: 'no browser when setup failed');
  });

  uiTest('already signed in: Keep Current Sign-in skips the browser', (tester) async {
    final t = fresh(builtin: true, signedIn: true);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    await tap(tester, 'Continue');
    expect(find.text('Keep Current Sign-in'), findsOneWidget);
    await tap(tester, 'Keep Current Sign-in');
    expect(find.text("You're all set"), findsOneWidget);
    expect(t.engine.logins, isEmpty);
  });

  uiTest('if the service cannot be registered, Try Again retries', (tester) async {
    final t = fresh(builtin: true);
    t.engine.result = (c) => c == 'service enable'
        ? const ActionResult(false, 'could not register')
        : const ActionResult(true, 'ok');
    t.engine.service = const ServiceStatus(running: false, startsAtSignIn: false);
    await pumpApp(tester, t);
    await tap(tester, 'Get Started');
    await tap(tester, 'Continue');
    await tap(tester, 'Sign In with Google');
    t.engine.logins.single.urlC.complete('https://a.example/auth');
    t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
    await tester.settle();
    expect(find.text("CloudDrop couldn't start"), findsOneWidget);
    expect(find.text('could not register'), findsOneWidget);
    t.engine.result = (c) => const ActionResult(true, 'ok');
    await tap(tester, 'Try Again');
    expect(find.text('CloudDrop is running'), findsOneWidget);
  });

  uiTest('the main window appears once the engine reports a signed-in, configured app', (
    tester,
  ) async {
    final t = fresh(builtin: true);
    await pumpApp(tester, t);
    expect(t.controller.setupActive, isTrue);
    expect(find.text('Welcome to CloudDrop'), findsOneWidget);
    // (Another process finishing setup does not pull the user out of the assistant.)
    signedInEngine(t);
    await t.controller.poll(forceEngine: true);
    await tester.settle();
    expect(find.text('Welcome to CloudDrop'), findsOneWidget);
  });
}
