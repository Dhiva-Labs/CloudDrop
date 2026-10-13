/// Golden screenshots of the setup assistant, every page, light and dark, wide and narrow.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/harness.dart';
import '../support/scenarios.dart';

TestApp fresh({bool builtin = false, bool signedIn = false}) {
  final t = TestApp(
    windows: true,
    status: EngineStatus(configured: false, hasBuiltinClient: builtin, signedIn: signedIn),
  );
  t.engine.config = null;
  return t;
}

typedef Act = Future<void> Function(WidgetTester tester, TestApp t);

Future<void> tapText(WidgetTester tester, String text) async {
  final f = find.text(text);
  await tester.ensureVisible(f);
  await tester.tap(f);
  await tester.settle();
}

Future<void> fillKey(WidgetTester tester) async {
  final boxes = find.byType(TextBox);
  await tester.enterText(boxes.at(0), '123456789012-abcdefghijk.apps.googleusercontent.com');
  await tester.enterText(find.byType(PasswordBox), 'GOCSPX-not-a-real-secret');
  await tester.settle();
}

void setup(String name, TestApp Function() build, {Act? act, double? tall}) {
  for (final v in variants) {
    testWidgets('$name ${v.label}', (tester) async {
      final t = build();
      final size = tall == null
          ? v.size
          : Size(v.size.width, v.size.width < 600 ? tall * 1.35 : tall);
      await pumpApp(tester, t, size: size, brightness: v.brightness);
      if (act != null) {
        await act(tester, t);
        await tester.settle();
      }
      await expectLater(
        find.byType(FluentApp),
        matchesGoldenFile('../goldens/setup_${name}_${v.label}.png'),
      );
    });
  }
}

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  setup('welcome', fresh);

  setup(
    'key',
    fresh,
    tall: 1250,
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
    },
  );

  setup(
    'key_filled',
    fresh,
    tall: 1250,
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await fillKey(tester);
    },
  );

  setup(
    'key_bad_file',
    fresh,
    tall: 1250,
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tester.enterText(find.byType(TextBox).first, '{ not json');
      await tester.settle();
    },
  );

  setup(
    'folder',
    () => fresh(builtin: true),
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
    },
  );

  setup(
    'signin',
    () => fresh(builtin: true),
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
    },
  );

  setup(
    'signin_waiting',
    () => fresh(builtin: true),
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
      await tapText(tester, 'Sign In with Google');
      t.engine.logins.single.urlC.complete(
        'https://accounts.google.com/o/oauth2/v2/auth?client_id=123',
      );
      await tester.settle();
    },
  );

  // After Settings > Sign out: straight on the sign-in page, key and folder remembered.
  setup('signin_after_signout', () {
    final t = TestApp(
      windows: true,
      config: busyConfig,
      status: const EngineStatus(configured: true, signedIn: false),
    );
    t.controller.setupAtSignIn = true;
    return t;
  });

  setup(
    'signin_failed',
    () => fresh(builtin: true),
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
      await tapText(tester, 'Sign In with Google');
      t.engine.logins.single.urlC.complete(
        'https://accounts.google.com/o/oauth2/v2/auth?client_id=123',
      );
      t.engine.logins.single.doneC.complete(
        const ActionResult(false, 'Google said: access_denied'),
      );
      await tester.settle();
    },
  );

  setup(
    'signin_keep_current',
    () => fresh(builtin: true, signedIn: true),
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
    },
  );

  setup(
    'done',
    () => fresh(builtin: true),
    tall: 900,
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
      await tapText(tester, 'Sign In with Google');
      t.engine.logins.single.urlC.complete(
        'https://accounts.google.com/o/oauth2/v2/auth?client_id=123',
      );
      t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
      await tester.settle();
    },
  );

  setup(
    'done_failed',
    () {
      final t = fresh(builtin: true);
      t.engine.result = (c) => c == 'service enable'
          ? const ActionResult(false, 'could not register CloudDrop to start at sign-in')
          : const ActionResult(true, 'ok');
      t.engine.service = const ServiceStatus(running: false, startsAtSignIn: false);
      return t;
    },
    tall: 900,
    act: (tester, t) async {
      await tapText(tester, 'Get Started');
      await tapText(tester, 'Continue');
      await tapText(tester, 'Sign In with Google');
      t.engine.logins.single.urlC.complete(
        'https://accounts.google.com/o/oauth2/v2/auth?client_id=123',
      );
      t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
      await tester.settle();
    },
  );
}
