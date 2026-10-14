@Tags(['store'])
library;

/// The Microsoft Store listing screenshots: real app screens with the test fakes and a believable
/// sample folder (store_data.dart), light theme, wide layout, 1920x1080, pinned clock, the fonts
/// in test/fonts. The PNGs are golden files in winapp/store/screenshots:
///
///     flutter test --tags store --update-goldens test/store
///
/// The window is 1920x1080 physical pixels at 150% scaling (1280x720 logical), the most common
/// Windows laptop setting: the screens look the way people see them, not tiny in a huge window.
///
/// Not part of the normal test run: unless goldens are being updated the tests skip themselves
/// (dart_test.yaml explains why this isn't a config-level exclude).

import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/logic/files_model.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/harness.dart';
import 'store_data.dart';

const Size _pixels = Size(1920, 1080);
const double _scale = 1.5;

typedef Act = Future<void> Function(WidgetTester tester, TestApp t);

void shot(String file, {AppPane pane = AppPane.home, Act? act}) {
  testWidgets(
    file,
    (tester) async {
      final t = storeApp();
      t.controller.pane.value = pane;
      await pumpApp(
        tester,
        t,
        size: _pixels,
        devicePixelRatio: _scale,
        brightness: Brightness.light,
      );
      if (act != null) {
        await act(tester, t);
        await tester.settle();
      }
      await expectLater(
        find.byType(FluentApp),
        matchesGoldenFile('../../store/screenshots/$file.png'),
      );
    },
    // Only made on request: flutter test --tags store --update-goldens test/store
    skip: !autoUpdateGoldenFiles,
  );
}

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  // 1. Home, uploading a few photos.
  shot('01-home');

  // 2. Files at the top: folders, and files that are uploaded, only in Google Drive, uploading.
  shot('02-files', pane: AppPane.files);

  // 3. Inside Photos/2026, newest first, one photo selected: the command bar.
  shot(
    '03-files-folder',
    pane: AppPane.files,
    act: (tester, t) async {
      final files = scopeOf(tester).files..openDir('Photos/2026');
      await tester.settle();
      files
        ..setSort(const Sort(SortKey.date, true))
        ..selectOnly('Photos/2026/IMG_5120.jpg');
    },
  );

  // 4. Activity of today and yesterday.
  shot('04-activity', pane: AppPane.activity);

  // 5. Settings: free up space after 30 days, one kept folder.
  shot('05-settings', pane: AppPane.settings);
}
