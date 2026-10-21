/// Golden screenshots of several folders (connections): the switcher with each folder's status
/// dot, the "Remove This Folder…" dialog and the assistant that adds another folder; light and
/// dark, wide (1100x760) and narrow (480 wide). Regenerate with
/// `flutter test --update-goldens test/ui`, then look at the PNGs in test/goldens/.
library;

import 'package:clouddrop_app/state/app_controller.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/harness.dart';
import '../support/scenarios.dart';

typedef Act = Future<void> Function(WidgetTester tester, TestApp t);

void shot(
  String name,
  TestApp Function() build, {
  AppPane pane = AppPane.home,
  Act? act,
  double? tall,
}) {
  for (final v in variants) {
    testWidgets('$name ${v.label}', (tester) async {
      final t = build();
      t.controller.pane.value = pane;
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
        matchesGoldenFile('../goldens/${name}_${v.label}.png'),
      );
    });
  }
}

Future<void> tapText(WidgetTester tester, String text) async {
  final f = find.text(text);
  await tester.ensureVisible(f);
  await tester.tap(f);
  await tester.settle();
}

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  // Home of the CloudDrop folder, with "Work" (OneDrive, uploading) beside it in the switcher.
  shot('home_two_folders', twoFoldersApp);

  // Home of "Work": the switcher's other entry is selected; the uploads name its top as "Work".
  shot('home_two_folders_work', () => twoFoldersApp(remember: 'Work'));

  // Settings › Remove This Folder… on "Work".
  shot(
    'settings_remove_dialog',
    () => twoFoldersApp(remember: 'Work'),
    pane: AppPane.settings,
    tall: 1400,
    act: (tester, t) async {
      // (No scrolling: the whole page is on screen.)
      await tester.tap(find.text('Remove This Folder…'));
    },
  );

  // "Add Another Folder…": the assistant starts on where to upload, with a way out.
  shot(
    'setup_add_folder',
    () {
      final t = twoFoldersApp();
      t.engine.providerList = allProviders;
      return t;
    },
    tall: 900,
    act: (tester, t) async {
      t.controller.beginAddConnection();
      await tester.settle();
    },
  );

  // Its folder page, a folder chosen: the name it gets, and the cloud folder named after it.
  shot(
    'setup_add_folder_chosen',
    () {
      final t = twoFoldersApp();
      t.system.directory = r'C:\Users\Dhiva\Documents\Family Photos';
      return t;
    },
    act: (tester, t) async {
      t.controller.beginAddConnection();
      await tester.settle();
      await tapText(tester, 'Continue');
      await tapText(tester, 'Choose…');
    },
  );

  // A folder that already uploads is refused before the engine is even asked.
  shot(
    'setup_add_folder_taken',
    () {
      final t = twoFoldersApp();
      t.system.directory = r'C:\Users\Dhiva\CloudDrop\Photos';
      return t;
    },
    act: (tester, t) async {
      t.controller.beginAddConnection();
      await tester.settle();
      await tapText(tester, 'Continue');
      await tapText(tester, 'Choose…');
    },
  );
}
