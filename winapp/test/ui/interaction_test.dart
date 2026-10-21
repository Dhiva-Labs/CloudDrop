/// Widget tests: every screen, driven like a user would, against fakes and an in-memory
/// copy of the engine's database.
library;

import 'dart:io';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/state/files_controller.dart';
import 'package:clouddrop_app/ui/app.dart';
import 'package:clouddrop_app/ui/pages/files_page.dart';
import 'package:clouddrop_app/ui/pages/settings_page.dart';
import 'package:desktop_drop/desktop_drop.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

import '../support/fakes.dart';
import '../support/harness.dart';
import '../support/scenarios.dart';

Future<void> tapText(
  WidgetTester tester,
  String text, {
  int index = 0,
  bool secondary = false,
}) async {
  final f = find.text(text).at(index);
  await tester.ensureVisible(f);
  await tester.tap(f, buttons: secondary ? kSecondaryButton : kPrimaryButton);
  await tester.settle();
}

Future<void> tapTooltip(WidgetTester tester, String tip) async {
  await tester.tap(find.byTooltip(tip).first);
  await tester.settle();
}

Future<void> withKey(
  WidgetTester tester,
  LogicalKeyboardKey key,
  Future<void> Function() body,
) async {
  await tester.sendKeyDownEvent(key);
  await body();
  await tester.sendKeyUpEvent(key);
}

Finder inMenu(String text) =>
    find.descendant(of: find.byType(MenuFlyout), matching: find.text(text));
Finder filesList() => find.descendant(of: find.byType(FilesPage), matching: find.byType(ListView));
Finder inList(String text) => find.descendant(of: filesList(), matching: find.text(text));

Finder inDialog(String text) =>
    find.descendant(of: find.byType(ContentDialog), matching: find.text(text));

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  otherProviders();

  group('shell', () {
    uiTest('a first start shows the setup assistant, not the main window', (tester) async {
      final t = TestApp(windows: true, status: const EngineStatus.unconfigured());
      t.engine.config = null;
      await pumpApp(tester, t);
      expect(find.text('Welcome to CloudDrop'), findsOneWidget);
      expect(find.text('Home'), findsNothing);
    });

    uiTest('a set-up app opens straight on Home with four screens', (tester) async {
      await pumpApp(tester, busyApp());
      for (final label in ['Home', 'Files', 'Activity', 'Settings']) {
        expect(find.text(label), findsWidgets, reason: label);
      }
      expect(find.text('Uploading 3 files'), findsOneWidget);
    });

    uiTest('navigation, narrow window: the same four screens along the top', (tester) async {
      final t = busyApp();
      await pumpApp(tester, t, size: narrow);
      expect(find.text('Home'), findsWidgets);
      await tester.tap(find.text('Files'));
      await tester.settle();
      expect(t.controller.pane.value, AppPane.files);
      expect(find.text('Search all files'), findsOneWidget);
      await tester.tap(find.text('Activity'));
      await tester.settle();
      expect(t.controller.pane.value, AppPane.activity);
    });

    uiTest('an engine that cannot be started gets an explanation and a retry', (tester) async {
      final t = busyApp();
      t.engine.statusError = CliUnavailable('clouddrop.exe', 'not found');
      await pumpApp(tester, t);
      expect(find.text("CloudDrop can't start its engine"), findsOneWidget);
      expect(find.textContaining('not found'), findsOneWidget);
      t.engine.statusError = null;
      await tester.tap(find.text('Try Again'));
      await tester.settle();
      expect(find.text('Uploading 3 files'), findsOneWidget);
    });

    uiTest('an engine that complains about its settings says so, without blaming the exe', (
      tester,
    ) async {
      final t = busyApp();
      t.engine.statusError = StateError('invalid config C:\\x\\config.toml: expected newline');
      await pumpApp(tester, t);
      expect(find.text("CloudDrop can't read its state"), findsOneWidget);
      expect(find.textContaining('invalid config'), findsOneWidget);
      expect(find.textContaining('Settings file:'), findsOneWidget);
      expect(find.textContaining('needs clouddrop.exe'), findsNothing);
    });

    uiTest('a settings file that cannot be read gets an error page, not the setup assistant', (
      tester,
    ) async {
      const error =
          r'invalid config C:\Users\Dhiva\AppData\Roaming\clouddrop\config.toml: TOML parse error at line 4, column 12'
          '\n  |\n4 | sync_dir = C:\\Users\n  |            ^\ninvalid escape\n';
      final t = TestApp(windows: true, status: const EngineStatus.unconfigured(configError: error));
      t.engine.config = null;
      await pumpApp(tester, t);
      expect(find.text("CloudDrop's settings can't be read"), findsOneWidget);
      // Only the first line of the error, which names the file.
      expect(find.textContaining('TOML parse error at line 4, column 12'), findsOneWidget);
      expect(find.textContaining('invalid escape'), findsNothing);
      expect(find.textContaining('Nothing was changed'), findsOneWidget);
      expect(find.text('Welcome to CloudDrop'), findsNothing);
      expect(find.text('Home'), findsNothing);
      expect(find.text('Try Again'), findsOneWidget);
      expect(find.text('Open Settings Folder'), findsOneWidget);

      await tester.tap(find.text('Open Settings Folder'));
      await tester.pump();
      expect(t.system.opened, [r'C:\Users\Dhiva\AppData\Roaming\clouddrop']);
      expect(t.engine.calls, isEmpty, reason: 'nothing was run that could touch the file');

      // Still broken: Try Again stays on the page.
      await tester.tap(find.text('Try Again'));
      await tester.settle();
      expect(find.text("CloudDrop's settings can't be read"), findsOneWidget);

      // Fixed by hand: Try Again leads on (this folder was never set up: the assistant).
      t.engine.statusValue = const EngineStatus.unconfigured(hasBuiltinClient: true);
      await tester.tap(find.text('Try Again'));
      await tester.settle();
      expect(find.text("CloudDrop's settings can't be read"), findsNothing);
      expect(find.text('Welcome to CloudDrop'), findsOneWidget);
    });

    uiTest('a settings error that does not name the file shows the path itself', (tester) async {
      final t = TestApp(
        windows: true,
        status: const EngineStatus.unconfigured(configError: 'cannot read the file: access denied'),
      );
      t.engine.config = null;
      await pumpApp(tester, t);
      expect(find.text('cannot read the file: access denied'), findsOneWidget);
      expect(
        find.textContaining(r'Settings file: C:\Users\Dhiva\AppData\Roaming\clouddrop\config.toml'),
        findsOneWidget,
      );
    });

    uiTest('messages appear as InfoBars', (tester) async {
      final t = busyApp();
      await pumpApp(tester, t);
      t.controller.say('Uploads paused.', Severity.success);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('Uploads paused.'), findsOneWidget);
      expect(find.byType(InfoBar), findsWidgets);
      await tester.pump(const Duration(seconds: 6));
      await tester.pump(const Duration(seconds: 1));
    });
  });

  group('Home', () {
    uiTest('failed uploads: the banner\'s Review button opens Files on Failed', (tester) async {
      final t = busyApp();
      await pumpApp(tester, t);
      expect(find.text("2 files couldn't be uploaded"), findsOneWidget);
      await tester.tap(find.text('Review'));
      await tester.settle();
      expect(t.controller.pane.value, AppPane.files);
      expect(find.text('Failed'), findsWidgets);
      expect(find.text('Quarterly report.pdf'), findsOneWidget);
      expect(find.text('Archive.7z'), findsOneWidget);
      expect(find.text('notes.txt'), findsNothing);
      expect(find.text('Retry All'), findsOneWidget);
    });

    uiTest('no banner when nothing failed', (tester) async {
      await pumpApp(tester, calmApp());
      expect(find.text('Everything is uploaded'), findsOneWidget);
      expect(find.textContaining("couldn't be uploaded"), findsNothing);
      expect(find.text('Uploading now'), findsNothing);
    });

    uiTest('uploading now lists each file with its progress', (tester) async {
      await pumpApp(tester, busyApp());
      expect(find.text('Uploading now'), findsOneWidget);
      expect(find.text('IMG_20261012_184501.jpg'), findsOneWidget);
      expect(find.text('6.2 MB / 9.6 MB'), findsOneWidget);
      expect(find.text('120.0 MB / 410.0 MB'), findsOneWidget);
    });

    uiTest('storage cards: Google Drive quota and this computer', (tester) async {
      await pumpApp(tester, busyApp());
      expect(find.text('11.2 GB of 15.0 GB used'), findsOneWidget);
      expect(find.textContaining('CloudDrop holds'), findsOneWidget);
      expect(find.text('2.1 GB freed'), findsOneWidget);
      expect(find.textContaining('still stored here'), findsOneWidget);
    });

    uiTest('storage card for unlimited accounts and before the quota is known', (tester) async {
      final t = calmApp();
      t.statusSource.value = DaemonStatus(
        pid: 1,
        updatedMs: nowMs,
        phase: Phase.idle,
        quota: const Quota(usage: 3 * gb, usageInDrive: gb),
      );
      await pumpApp(tester, t);
      expect(find.text('3.0 GB used'), findsOneWidget);
      expect(find.textContaining('unlimited storage'), findsOneWidget);

      final t2 = calmApp();
      t2.statusSource.value = DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.idle);
      await pumpApp(tester, t2);
      expect(find.text('Checking…'), findsOneWidget);
    });

    uiTest('an empty folder says so on the computer card', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      await pumpApp(tester, t);
      expect(find.text('Nothing yet'), findsOneWidget);
      expect(find.text('Files you add to CloudDrop show up here'), findsOneWidget);
      expect(find.text('Put files in your CloudDrop folder to back them up.'), findsOneWidget);
    });

    uiTest('quick actions: Open Folder, Add Files, Pause', (tester) async {
      final t = calmApp();
      await pumpApp(tester, t);
      await tester.tap(find.text('Open Folder'));
      await tester.pump();
      expect(t.system.opened, [r'C:\Users\Dhiva\CloudDrop']);

      t.system.files = const [];
      await tester.tap(find.text('Add Files…'));
      await tester.pump();

      await tester.tap(find.text('Pause Uploads'));
      await tester.settle();
      expect(t.engine.calls, contains('pause'));
    });

    uiTest('paused: the tile and the hero both offer Resume', (tester) async {
      final t = busyApp(
        daemon: DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.paused, waiting: 3),
      );
      t.db.meta('paused', '1');
      await pumpApp(tester, t);
      expect(find.text('Uploads are paused'), findsOneWidget);
      expect(find.text('Resume Uploads'), findsNWidgets(2));
      await tester.tap(find.text('Resume Uploads').first);
      await tester.settle();
      expect(t.engine.calls, contains('resume'));
    });

    uiTest('Free Up Space asks first, and only then frees everything', (tester) async {
      final t = calmApp();
      await pumpApp(tester, t);
      await tester.tap(find.text('Free Up Space…'));
      await tester.settle();
      expect(find.text('Free up space?'), findsOneWidget);
      expect(
        find.textContaining('removes the local copies of all 5 uploaded files'),
        findsOneWidget,
      );
      await tester.tap(find.text('Cancel'));
      await tester.settle();
      expect(t.engine.calls.where((c) => c.startsWith('free')), isEmpty);

      await tester.tap(find.text('Free Up Space…'));
      await tester.settle();
      await tester.tap(inDialog('Free Up Space'));
      await tester.settle();
      expect(t.engine.calls, [r'free C:\Users\Dhiva\CloudDrop']);
    });

    uiTest('the Free Up Space tile is off when nothing is uploaded', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      t.db.file('only-cloud.zip', state: 'cloud_only', cloudSize: 5);
      await pumpApp(tester, t);
      await tester.tap(find.text('Free Up Space…'));
      await tester.settle();
      expect(find.text('Free up space?'), findsNothing);
    });

    uiTest('not running: Start CloudDrop', (tester) async {
      final t = calmApp();
      t.statusSource.value = null;
      await pumpApp(tester, t);
      expect(find.text("CloudDrop isn't running"), findsOneWidget);
      await tester.tap(find.text('Start CloudDrop'));
      await tester.settle();
      expect(t.engine.calls, contains('service start'));
    });

    uiTest('sign-in needed: the dialog shows the link while waiting, then closes', (tester) async {
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

      final t = calmApp();
      t.statusSource.value = DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.needsSignIn);
      await pumpApp(tester, t);
      expect(find.text('Sign in again'), findsWidgets);
      await tester.tap(find.widgetWithText(FilledButton, 'Sign In'));
      await tester.settle();
      expect(find.text('Sign in to Google'), findsOneWidget);
      expect(find.text('Waiting for you to finish in the browser…'), findsOneWidget);
      final login = t.engine.logins.single;
      login.urlC.complete('https://accounts.google.com/auth?x=1');
      await tester.settle();
      expect(t.system.urls, [
        'https://accounts.google.com/auth?x=1',
      ], reason: 'the browser is opened for you');
      await tester.tap(find.text('Copy sign-in link'));
      await tester.settle();
      expect(clip, 'https://accounts.google.com/auth?x=1');
      expect(find.text('Link copied'), findsOneWidget);
      login.doneC.complete(const ActionResult(true, 'Signed in.'));
      await tester.settle();
      expect(find.text('Sign in to Google'), findsNothing, reason: 'closed once signed in');
    });

    uiTest('cancelling the sign-in dialog stops waiting', (tester) async {
      final t = calmApp();
      t.statusSource.value = DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.needsSignIn);
      await pumpApp(tester, t);
      await tester.tap(find.widgetWithText(FilledButton, 'Sign In'));
      await tester.settle();
      await tester.tap(find.text('Cancel'));
      await tester.settle();
      expect(t.engine.logins.single.cancelled, isTrue);
      expect(find.text('Sign in to Google'), findsNothing);
    });

    uiTest('open in Google Drive from the storage card', (tester) async {
      final t = calmApp();
      await pumpApp(tester, t);
      await tester.tap(find.byTooltip('Open in Google Drive'));
      await tester.pump();
      expect(t.system.urls, ['https://drive.google.com/drive/folders/ROOT']);
    });
  });

  group('Files', () {
    Future<FilesCtx> files(WidgetTester tester, {Size size = wide, TestApp? app}) async {
      final t = app ?? busyApp();
      t.controller.pane.value = AppPane.files;
      await pumpApp(tester, t, size: size);
      return FilesCtx(t, scopeOf(tester).files);
    }

    uiTest('lists folders first, with status, size and date', (tester) async {
      await files(tester);
      final names = [
        'Documents',
        'Photos',
        'Videos',
        'Work',
        'Archive.7z',
        'backup-2026-10.zip',
        'notes.txt',
        'Quarterly report.pdf',
        'setup-guide.md',
      ];
      for (final n in names) {
        expect(find.text(n), findsOneWidget, reason: n);
      }
      final ys = [for (final n in names) tester.getTopLeft(find.text(n)).dy];
      expect(ys, orderedEquals([...ys]..sort()), reason: 'sorted: folders, then by name');
      expect(find.text('Uploaded'), findsOneWidget);
      expect(find.text('Failed'), findsNWidgets(2));
      expect(find.text('Waiting'), findsOneWidget);
      expect(find.text('Uploading'), findsOneWidget);
      expect(find.text('6 files · 2 waiting · 2 cloud-only'), findsOneWidget);
      expect(find.text('2 files · all cloud-only'), findsOneWidget);
      expect(find.text('410.0 MB'), findsOneWidget);
      expect(find.text('Yesterday'), findsOneWidget);
      expect(find.text('21 files · 2.5 GB · 6 only in Google Drive'), findsOneWidget);
    });

    uiTest('clicking selects; the command bar shows what the selection allows', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'setup-guide.md');
      expect(find.text('Free Up Space'), findsOneWidget, reason: 'an uploaded file can be freed');
      expect(find.text('Download'), findsNothing);
      expect(find.text('Retry'), findsNothing);
      await tapText(tester, 'Quarterly report.pdf');
      expect(find.text('Retry'), findsOneWidget);
      expect(find.text('Free Up Space'), findsNothing);
      await tapText(tester, 'Videos');
      expect(find.text('Download'), findsOneWidget);
      expect(find.byTooltip('Always keep on this computer'), findsOneWidget);
      expect(t.c.selectedPaths, {'Videos'});
    });

    uiTest('one selected file offers Open first', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'setup-guide.md');
      expect(find.text('Open'), findsOneWidget);
      await tester.tap(find.text('Open'));
      await tester.pump();
      expect(t.app.system.opened, [r'C:\Users\Dhiva\CloudDrop\setup-guide.md']);
      await tapText(tester, 'Videos');
      expect(find.text('Open'), findsNothing, reason: 'folders open by double-click');
      await tapText(tester, 'notes.txt');
      await withKey(
        tester,
        LogicalKeyboardKey.controlLeft,
        () => tapText(tester, 'setup-guide.md'),
      );
      expect(find.text('Open'), findsNothing, reason: 'one file at a time');
    });

    uiTest('Windows: a freed file that is still on disk opens; a deleted one offers Download', (
      tester,
    ) async {
      final app = busyApp(cloudFiles: true);
      const root = r'C:\Users\Dhiva\CloudDrop';
      app.onDisk.add('$root\\Videos\\Family reunion.mp4'); // the placeholder Explorer shows
      final t = await files(tester, app: app);
      t.c.openDir('Videos');
      await tester.settle();

      // Has a placeholder: Open is the primary action next to Download.
      await tapText(tester, 'Family reunion.mp4');
      await tester.settle();
      expect(find.text('Open'), findsOneWidget);
      expect(find.text('Download'), findsOneWidget);
      await tester.tap(find.text('Open'));
      await tester.pump();
      expect(app.system.opened, ['$root\\Videos\\Family reunion.mp4']);

      // Double-click does the same, with no "only in Google Drive" detour.
      await tester.tap(inList('Family reunion.mp4'));
      await tester.pump(const Duration(milliseconds: 50));
      await tester.tap(inList('Family reunion.mp4'));
      await tester.settle();
      expect(app.system.opened.length, 2);
      expect(find.text('“Family reunion.mp4” is only in Google Drive'), findsNothing);

      // Deleted locally: no Open, and double-click offers the download.
      await tapText(tester, 'Screen recording.mkv');
      await tester.settle();
      expect(find.text('Open'), findsNothing);
      expect(find.text('Download'), findsOneWidget);
      await tester.tap(inList('Screen recording.mkv'));
      await tester.pump(const Duration(milliseconds: 50));
      await tester.tap(inList('Screen recording.mkv'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('“Screen recording.mkv” is only in Google Drive'), findsOneWidget);
      await tester.tap(find.text('Download').last);
      await tester.settle();
      expect(app.engine.calls.last, 'restore $root\\Videos\\Screen recording.mkv');
      expect(app.system.opened.length, 2);
    });

    uiTest('Ctrl-click toggles, Shift-click selects a range, Esc clears', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'Documents');
      await withKey(tester, LogicalKeyboardKey.controlLeft, () => tapText(tester, 'Work'));
      expect(t.c.selectedPaths, {'Documents', 'Work'});
      expect(find.text('2 selected'), findsOneWidget);
      await withKey(tester, LogicalKeyboardKey.controlLeft, () => tapText(tester, 'Documents'));
      expect(t.c.selectedPaths, {'Work'});
      await tapText(tester, 'Photos');
      await withKey(tester, LogicalKeyboardKey.shiftLeft, () => tapText(tester, 'Work'));
      expect(t.c.selectedPaths, {'Photos', 'Videos', 'Work'});
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await tester.settle();
      expect(t.c.selectedPaths, isEmpty);
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.keyA);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.settle();
      expect(t.c.selectedPaths.length, 9);
    });

    uiTest('double-click opens a folder; breadcrumbs and Back navigate', (tester) async {
      final t = await files(tester);
      await tester.tap(find.text('Documents'));
      await tester.pump(const Duration(milliseconds: 50));
      await tester.tap(find.text('Documents'));
      await tester.settle();
      expect(t.c.dir, 'Documents');
      expect(find.text('Budget 2026.xlsx'), findsOneWidget);
      expect(find.text('Photos'), findsNothing);

      t.c.openDir('Documents/Invoices');
      await tester.settle();
      expect(find.text('Invoice 0042.pdf'), findsOneWidget);
      // Breadcrumb: CloudDrop > Documents > Invoices
      await tapText(tester, 'Documents');
      expect(t.c.dir, 'Documents');
      await tester.tap(find.byTooltip('Back to the parent folder (Alt+←)'));
      await tester.settle();
      expect(t.c.dir, '');
      expect(t.c.selectedPaths, {'Documents'}, reason: 'the folder you left is selected');
    });

    uiTest('keyboard: Backspace goes up, Enter opens', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'Photos');
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await tester.settle();
      expect(t.c.dir, 'Photos');
      await tester.sendKeyEvent(LogicalKeyboardKey.backspace);
      await tester.settle();
      expect(t.c.dir, '');
    });

    uiTest('search looks in every folder and shows where each hit lives', (tester) async {
      final t = await files(tester);
      await tester.enterText(find.byType(TextBox).first, 'invoice');
      await tester.settle();
      expect(find.text('Search “invoice”'), findsOneWidget);
      expect(find.text('Invoice 0043.pdf'), findsOneWidget);
      expect(find.text('Documents/Invoices'), findsWidgets);
      expect(find.text('Documents'), findsWidgets);
      expect(t.c.flat, isTrue);
      // Clear it with the button.
      await tester.tap(find.byIcon(FluentIcons.clear).first);
      await tester.settle();
      expect(t.c.search, '');
      expect(find.text('Videos'), findsOneWidget);
    });

    uiTest('no results', (tester) async {
      await files(tester);
      await tester.enterText(find.byType(TextBox).first, 'qqqq');
      await tester.settle();
      expect(find.text('No Results'), findsOneWidget);
      expect(find.text('Nothing in CloudDrop is named like “qqqq”.'), findsOneWidget);
    });

    uiTest('filter: Cloud-only shows files across all folders', (tester) async {
      final t = await files(tester);
      await tester.tap(find.byType(ComboBox<Filter>));
      await tester.settle();
      await tester.tap(find.text('Cloud-only').last);
      await tester.settle();
      expect(t.c.filter, Filter.cloudOnly);
      expect(find.text('Family reunion.mp4'), findsOneWidget);
      expect(find.text('Invoice 0043.pdf'), findsOneWidget);
      expect(find.text('Cloud-only'), findsWidgets);
      expect(find.text('notes.txt'), findsNothing);
    });

    uiTest('filter with nothing to show says why', (tester) async {
      final t = await files(tester, app: calmApp());
      t.c.setFilter(Filter.failed);
      await tester.settle();
      expect(find.text('No failed uploads'), findsOneWidget);
      expect(find.text('Every file here was uploaded successfully.'), findsOneWidget);
    });

    uiTest('sorting by clicking the column headers', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'Size');
      expect(t.c.sort.key.name, 'size');
      expect(t.c.sort.descending, isFalse);
      await tapText(tester, 'Size');
      expect(t.c.sort.descending, isTrue);
      // Folders stay first, then the biggest file.
      final biggest = tester.getTopLeft(find.text('backup-2026-10.zip')).dy;
      final smallest = tester.getTopLeft(find.text('notes.txt')).dy;
      expect(biggest, lessThan(smallest));
      await tapText(tester, 'Status');
      expect(t.c.sort.key.name, 'status');
      await tapText(tester, 'Modified');
      expect(t.c.sort.key.name, 'date');
      await tapText(tester, 'Name');
      expect(t.c.sort.key.name, 'name');
    });

    uiTest('right-click selects the row and opens the menu with the same actions', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'Videos', secondary: true);
      expect(t.c.selectedPaths, {'Videos'});
      for (final item in [
        'Open',
        'Download',
        'Open in Google Drive',
        'Show in File Explorer',
        'Always Keep on This Computer',
      ]) {
        expect(inMenu(item), findsOneWidget, reason: item);
      }
      expect(inMenu('Free Up Space'), findsNothing, reason: 'all cloud-only: nothing to free');
      await tester.tap(inMenu('Download'));
      await tester.settle();
      expect(t.app.engine.calls, [r'restore C:\Users\Dhiva\CloudDrop\Videos']);
    });

    uiTest('context menu on a failed file offers Retry Upload', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'Quarterly report.pdf', secondary: true);
      expect(inMenu('Retry Upload'), findsOneWidget);
      await tester.tap(inMenu('Retry Upload'));
      await tester.settle();
      expect(t.app.engine.calls, [r'retry C:\Users\Dhiva\CloudDrop\Quarterly report.pdf']);
    });

    uiTest('Free Up Space from the command bar asks first', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'setup-guide.md');
      await tester.tap(find.text('Free Up Space'));
      await tester.settle();
      expect(find.textContaining('removes the local copy of “setup-guide.md”'), findsOneWidget);
      await tester.tap(inDialog('Free Up Space'));
      await tester.settle();
      expect(t.app.engine.calls, [r'free C:\Users\Dhiva\CloudDrop\setup-guide.md']);

      await tapText(tester, 'Work');
      await tester.tap(find.text('Free Up Space'));
      await tester.settle();
      expect(find.textContaining('uploaded files in “Work”'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await tester.settle();
      expect(t.app.engine.calls.length, 1, reason: 'cancelled');
    });

    uiTest('Download, pin, Open in Google Drive and Show in Explorer from the command bar', (
      tester,
    ) async {
      final t = await files(tester);
      await tapText(tester, 'Videos');
      await tester.tap(find.text('Download'));
      await tester.settle();
      await tester.tap(find.byTooltip('Always keep on this computer'));
      await tester.settle();
      await tester.tap(find.byTooltip('Open in Google Drive'));
      await tester.pump();
      await tester.tap(find.byTooltip('Show in File Explorer'));
      await tester.pump();
      expect(t.app.engine.calls, [
        r'restore C:\Users\Dhiva\CloudDrop\Videos',
        r'keep C:\Users\Dhiva\CloudDrop\Videos',
      ]);
      expect(t.app.system.urls, ['https://drive.google.com/drive/folders/id-Videos']);
      expect(t.app.system.revealed.length, 1);
    });

    uiTest('a pinned folder shows its pin, and the toggle unpins it', (tester) async {
      final t = await files(tester);
      expect(find.byIcon(FluentIcons.pinned_solid), findsOneWidget, reason: 'Work is pinned');
      await tapText(tester, 'Work');
      await tester.tap(find.byTooltip('Always keep on this computer'));
      await tester.settle();
      expect(t.app.engine.calls, [r'unkeep C:\Users\Dhiva\CloudDrop\Work']);
    });

    uiTest('Retry All on the Failed filter', (tester) async {
      final t = await files(tester);
      t.c.setFilter(Filter.failed);
      await tester.settle();
      await tester.tap(find.text('Retry All'));
      await tester.settle();
      expect(t.app.engine.calls, [
        'retry',
      ], reason: 'at the top, "retry" with no path means everything');
      t.c.openDir('Photos');
      await tester.settle();
      t.c.setFilter(Filter.failed);
      await tester.settle();
      expect(find.text('Retry All'), findsNothing, reason: 'nothing failed in Photos');
    });

    uiTest('cloud-only file: opening offers a download', (tester) async {
      final t = await files(tester);
      t.c.openDir('Documents/Invoices');
      await tester.settle();
      await tester.tap(find.text('Invoice 0043.pdf'));
      await tester.pump(const Duration(milliseconds: 50));
      await tester.tap(find.text('Invoice 0043.pdf'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.text('“Invoice 0043.pdf” is only in Google Drive'), findsOneWidget);
      await tester.tap(find.text('Download'));
      await tester.settle();
      expect(t.app.engine.calls, [
        r'restore C:\Users\Dhiva\CloudDrop\Documents\Invoices\Invoice 0043.pdf',
      ]);
      await tester.pump(const Duration(seconds: 9));
    });

    uiTest('empty folder offers Add Files', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      final h = await files(tester, app: t);
      expect(find.text('CloudDrop Is Empty'), findsOneWidget);
      expect(
        find.text('Drop files here or click Add Files to start uploading to Google Drive.'),
        findsOneWidget,
      );
      t.system.files = const [];
      await tester.tap(find.text('Add Files…'));
      await tester.pump();
      expect(h.c.dir, '');
    });

    uiTest('the list follows the database and keeps the selection', (tester) async {
      final t = await files(tester);
      await tapText(tester, 'notes.txt');
      t.app.db.file('aaa-new.txt', size: 10, mtimeMs: ago(minutes: 1));
      t.app.db.db.execute("DELETE FROM entries WHERE path = 'setup-guide.md'");
      t.app.controller.dbVersion.value++;
      await tester.settle();
      expect(inList('aaa-new.txt'), findsOneWidget);
      expect(inList('setup-guide.md'), findsNothing);
      expect(t.c.selectedPaths, {'notes.txt'}, reason: 'the selection survives a refresh');
      expect(inList('notes.txt'), findsOneWidget);
    });

    uiTest('a refresh keeps the scroll position', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      for (var i = 0; i < 80; i++) {
        t.db.file(
          'file-${i.toString().padLeft(3, '0')}.txt',
          size: 100 + i,
          mtimeMs: ago(days: i),
        );
      }
      final h = await files(tester, app: t);
      expect(find.text('file-000.txt'), findsOneWidget);
      await tester.drag(filesList(), const Offset(0, -900));
      await tester.settle();
      final listState = tester.state<ScrollableState>(
        find.descendant(of: filesList(), matching: find.byType(Scrollable)).first,
      );
      final offset = listState.position.pixels;
      expect(offset, greaterThan(500));
      t.db.file('file-zzz-new.txt', size: 1);
      t.db.db.execute("DELETE FROM entries WHERE path = 'file-005.txt'");
      t.controller.dbVersion.value++;
      await tester.settle();
      expect(h.c.items.length, 80);
      expect(listState.position.pixels, offset, reason: 'the list did not jump');
      expect(find.text('file-000.txt'), findsNothing);
    });

    uiTest('narrow window: size and date columns drop, actions stay reachable', (tester) async {
      final t = await files(tester, size: narrow);
      expect(find.text('Size'), findsNothing);
      expect(find.text('Modified'), findsNothing);
      expect(find.text('Status'), findsOneWidget);
      await tapText(tester, 'Quarterly report.pdf');
      expect(find.byTooltip('Retry'), findsWidgets);
      await tester.tap(find.byTooltip('Retry').first);
      await tester.settle();
      expect(t.app.engine.calls, [r'retry C:\Users\Dhiva\CloudDrop\Quarterly report.pdf']);
    });

    uiTest('Add Files copies what you pick into the open folder, without overwriting', (
      tester,
    ) async {
      final tmp = Directory.systemTemp.createTempSync('clouddrop-add');
      addTearDown(() => tmp.deleteSync(recursive: true));
      final src = File(p.join(tmp.path, 'src', 'report.pdf'))..createSync(recursive: true);
      src.writeAsStringSync('new');
      final sync = Directory(p.join(tmp.path, 'CloudDrop'))..createSync();
      Directory(p.join(sync.path, 'Docs')).createSync();
      File(p.join(sync.path, 'Docs', 'report.pdf')).writeAsStringSync('old');

      final t = TestApp(
        syncDir: sync.path,
        daemon: idleDaemon(),
        config: AppConfig(
          syncDir: sync.path,
          driveFolder: 'CloudDrop',
          clientId: '1.apps.googleusercontent.com',
          hasClientSecret: true,
        ),
      );
      t.db.dir('Docs');
      t.db.file('Docs/report.pdf', size: 3, mtimeMs: 1000);
      t.db.file('Docs/report (2).pdf', state: 'cloud_only', cloudSize: 3);
      t.controller.pane.value = AppPane.files;
      await pumpApp(tester, t);
      t.controller.showFiles(dir: 'Docs');
      await tester.settle();
      t.system.files = [src.path];
      final msgs = <AppMessage>[];
      t.controller.messages.listen(msgs.add);
      await tester.runAsync(() async {
        await tester.tap(find.text('Add Files'));
        await Future<void>.delayed(const Duration(milliseconds: 500));
      });
      await tester.settle();
      expect(File(p.join(sync.path, 'Docs', 'report.pdf')).readAsStringSync(), 'old');
      // "(2)" exists in Drive only, so the copy gets "(3)".
      expect(File(p.join(sync.path, 'Docs', 'report (3).pdf')).readAsStringSync(), 'new');
      expect(
        msgs.map((m) => m.text),
        contains('Copied 1 item to Docs. Uploading will start shortly.'),
      );
      await tester.pump(const Duration(seconds: 6));
    });

    uiTest('dropping files from Explorer copies them in', (tester) async {
      final tmp = Directory.systemTemp.createTempSync('clouddrop-drop');
      addTearDown(() => tmp.deleteSync(recursive: true));
      final src = File(p.join(tmp.path, 'src', 'photo.jpg'))..createSync(recursive: true);
      src.writeAsStringSync('img');
      final sync = Directory(p.join(tmp.path, 'CloudDrop'))..createSync();
      final t = TestApp(
        syncDir: sync.path,
        daemon: idleDaemon(),
        config: AppConfig(
          syncDir: sync.path,
          driveFolder: 'CloudDrop',
          clientId: '1.apps.googleusercontent.com',
          hasClientSecret: true,
        ),
      );
      t.controller.pane.value = AppPane.files;
      t.controller.pane.value = AppPane.files;
      tester.useWindow(wide);
      await t.ready();
      await tester.pumpWidget(
        CloudDropApp(
          app: t.controller,
          fontFamily: testFont,
          themeMode: ThemeMode.light,
          enableDrop: true,
        ),
      );
      await tester.settle();
      final target = tester.widget<DropTarget>(find.byType(DropTarget));
      target.onDragEntered?.call(
        DropEventDetails(localPosition: Offset.zero, globalPosition: Offset.zero),
      );
      await tester.pump();
      expect(find.text('Drop to add to CloudDrop'), findsOneWidget);
      await tester.runAsync(() async {
        target.onDragDone?.call(
          DropDoneDetails(
            files: [DropItemFile(src.path)],
            localPosition: Offset.zero,
            globalPosition: Offset.zero,
          ),
        );
        await Future<void>.delayed(const Duration(milliseconds: 500));
      });
      await tester.settle();
      expect(File(p.join(sync.path, 'photo.jpg')).readAsStringSync(), 'img');
      expect(find.text('Drop to add to CloudDrop'), findsNothing);
      await tester.pump(const Duration(seconds: 6));
    });
  });

  group('Activity', () {
    uiTest('grouped by day, newest first', (tester) async {
      final t = busyApp();
      t.controller.pane.value = AppPane.activity;
      await pumpApp(tester, t);
      final today = tester.getTopLeft(find.text('Today')).dy;
      final yesterday = tester.getTopLeft(find.text('Yesterday')).dy;
      expect(today, lessThan(yesterday));
      expect(find.text('Uploaded Budget 2026.xlsx'), findsOneWidget);
      expect(find.text("Couldn't upload Quarterly report.pdf"), findsOneWidget);
      expect(find.text('Renamed Resume.pdf'), findsOneWidget);
      expect(find.text('Moved IMG_2403.jpg'), findsOneWidget);
      expect(find.textContaining('from Photos/IMG_2403.jpg'), findsOneWidget);
      expect(find.text('Automatically freed up Invoice 0043.pdf'), findsOneWidget);
    });

    uiTest('clicking an item opens Files at that item', (tester) async {
      final t = busyApp();
      t.controller.pane.value = AppPane.activity;
      await pumpApp(tester, t);
      await tester.tap(find.text('Updated Project plan.docx'));
      await tester.settle();
      expect(t.controller.pane.value, AppPane.files);
      expect(inList('Project plan.docx'), findsOneWidget);
      expect(inList('Roadmap.pptx'), findsOneWidget);
      // The file is selected, and the command bar names it.
      expect(scopeOf(tester).files.selectedPaths, {'Work/Project plan.docx'});
      expect(scopeOf(tester).files.dir, 'Work');
    });

    uiTest('empty history', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      t.controller.pane.value = AppPane.activity;
      await pumpApp(tester, t);
      expect(find.text('No Activity Yet'), findsOneWidget);
    });

    uiTest('older activity is loaded on request', (tester) async {
      final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
      for (var i = 0; i < 130; i++) {
        t.db.activity('uploaded', 'f$i.txt', at: ago(hours: 200 - i), bytes: 1);
      }
      t.controller.pane.value = AppPane.activity;
      await pumpApp(tester, t);
      expect(find.text('Show Older Activity'), findsOneWidget);
      await tapText(tester, 'Show Older Activity');
      expect(find.text('Show Older Activity'), findsNothing);
      expect(find.text('Uploaded f0.txt'), findsOneWidget);
    });
  });

  group('Settings', () {
    Future<TestApp> settings(WidgetTester tester, {Size size = wide, TestApp? app}) async {
      final t = app ?? busyApp();
      t.controller.pane.value = AppPane.settings;
      // The whole page on screen (it has the folders' Add and Remove cards too).
      await pumpApp(tester, t, size: Size(size.width, 1450));
      return t;
    }

    uiTest('shows the current values', (tester) async {
      await settings(tester);
      expect(find.text('After 30 days'), findsOneWidget);
      expect(find.text('2 folders'), findsOneWidget);
      expect(find.text('Signed in'), findsOneWidget);
      expect(find.text(r'C:\Users\Dhiva\CloudDrop'), findsOneWidget);
      expect(find.text('My Drive → CloudDrop'), findsOneWidget);
      expect(find.text('Running'), findsOneWidget);
      expect(
        find.widgetWithText(
          TextBox,
          '.clouddrop-*, .goutputstream-*, *.part, *.crdownload, *.tmp, ~\$*, *.swp',
        ),
        findsOneWidget,
      );
      expect(find.text('Sign in again'), findsOneWidget);
      expect(find.text('Sign out'), findsOneWidget);
    });

    uiTest('automatic freeing: choices, mapped to config set', (tester) async {
      final t = await settings(tester);
      await tester.tap(find.byType(ComboBox<int>));
      await tester.settle();
      for (final label in [
        'Never',
        'Right after uploading',
        'After 7 days',
        'After 30 days',
        'After 90 days',
      ]) {
        expect(find.text(label), findsWidgets, reason: label);
      }
      await tester.tap(find.text('Right after uploading').last);
      await tester.settle();
      expect(t.engine.calls.last, 'config set auto_free_days now');
      await tester.tap(find.byType(ComboBox<int>));
      await tester.settle();
      await tester.tap(find.text('Never').last);
      await tester.settle();
      expect(t.engine.calls.last, 'config set auto_free_days never');
      await tester.tap(find.byType(ComboBox<int>));
      await tester.settle();
      await tester.tap(find.text('After 90 days').last);
      await tester.settle();
      expect(t.engine.calls.last, 'config set auto_free_days 90');
    });

    uiTest('a custom number of days is listed and selected', (tester) async {
      final t = busyApp(config: busyConfig.copyWith(autoFreeDays: 14));
      await settings(tester, app: t);
      expect(find.text('After 14 days'), findsOneWidget);
      await tester.tap(find.byType(ComboBox<int>));
      await tester.settle();
      // Between 7 and 30 days, in its place.
      expect(find.text('After 14 days'), findsWidgets);
    });

    uiTest('kept folders: remove one, add one inside the CloudDrop folder only', (tester) async {
      final t = await settings(tester);
      await tester.tap(find.byTooltip('Stop keeping this folder').first);
      await tester.settle();
      expect(t.engine.calls.last, r'unkeep C:\Users\Dhiva\CloudDrop\Work');

      t.system.directory = r'C:\Users\Dhiva\CloudDrop\Documents\Invoices';
      await tester.tap(find.text('Add Folder…'));
      await tester.settle();
      expect(t.engine.calls.last, r'keep C:\Users\Dhiva\CloudDrop\Documents\Invoices');

      // Outside the sync folder: refused with a sentence, nothing sent.
      final before = t.engine.calls.length;
      final msgs = <AppMessage>[];
      t.controller.messages.listen(msgs.add);
      t.system.directory = r'D:\Elsewhere';
      await tester.tap(find.text('Add Folder…'));
      await tester.settle();
      expect(t.engine.calls.length, before);
      expect(msgs.single.text, r'Choose a folder inside C:\Users\Dhiva\CloudDrop');

      // Already kept (inside a kept folder): says so.
      msgs.clear();
      t.system.directory = r'C:\Users\Dhiva\CloudDrop\Work\Sub';
      await tester.tap(find.text('Add Folder…'));
      await tester.settle();
      expect(msgs.single.text, '“Sub” is already kept on this computer');
      expect(t.engine.calls.length, before);
      await tester.pump(const Duration(seconds: 6));
    });

    uiTest('uploading options are written live', (tester) async {
      final t = await settings(tester);
      await tester.tap(find.byType(ToggleSwitch).first);
      await tester.settle();
      expect(t.engine.calls.last, 'config set notifications false');

      final field = find.descendant(
        of: find.byType(NumberBox<int>).first,
        matching: find.byType(TextBox),
      );
      await tester.enterText(field, '4');
      await tester.testTextInput.receiveAction(TextInputAction.done);
      await tester.pump(const Duration(milliseconds: 600));
      await tester.settle();
      expect(t.engine.calls, contains('config set parallel_uploads 4'));
    });

    uiTest('ignored patterns: Apply sends the list', (tester) async {
      final t = await settings(tester);
      final box = find.byType(TextBox).last;
      await tester.enterText(box, '*.bak,  *.old , ');
      await tester.tap(find.text('Apply'));
      await tester.settle();
      expect(t.engine.calls.last, 'config set ignore *.bak, *.old');
      expect(find.widgetWithText(TextBox, '*.bak, *.old'), findsOneWidget);
    });

    uiTest('a rejected value is explained, nothing else changes', (tester) async {
      final t = await settings(tester);
      t.engine.result = (c) => const ActionResult(false, 'ignore patterns must not be empty');
      final msgs = <AppMessage>[];
      t.controller.messages.listen(msgs.add);
      await tester.tap(find.text('Apply'));
      await tester.settle();
      expect(msgs.last.text, "Couldn't save: ignore patterns must not be empty");
      expect(msgs.last.severity, Severity.error);
      await tester.pump(const Duration(seconds: 6));
    });

    uiTest('background: start at sign-in and the service', (tester) async {
      final t = await settings(tester);
      final toggle = find.widgetWithText(Row, 'Start CloudDrop at sign-in');
      expect(toggle, findsWidgets);
      final switches = find.byType(ToggleSwitch);
      await tester.tap(switches.last);
      await tester.settle();
      expect(t.engine.calls.last, 'service disable');

      final t2 = calmApp();
      t2.statusSource.value = null;
      await settings(tester, app: t2);
      expect(
        find.descendant(of: find.byType(SettingsPage), matching: find.text('Not running')),
        findsOneWidget,
      );
      await tester.tap(find.text('Start'));
      await tester.settle();
      expect(t2.engine.calls.last, 'service start');
    });

    uiTest('sign in again; open the folders', (tester) async {
      final t = await settings(tester);
      await tester.tap(find.text('Sign in again'));
      await tester.settle();
      expect(find.text('Sign in to Google'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await tester.settle();
      await tester.tap(find.byTooltip('Open folder'));
      await tester.tap(find.byTooltip('Open in Google Drive'));
      await tester.pump();
      expect(t.system.opened, [r'C:\Users\Dhiva\CloudDrop']);
      expect(t.system.urls, ['https://drive.google.com/drive/folders/ROOT']);
    });

    uiTest('Sign out asks first, then signs out and leads back to the sign-in step', (
      tester,
    ) async {
      final t = await settings(tester);
      // `clouddrop logout` forgets the sign-in; the engine then reports signed_in: false.
      t.engine.result = (c) {
        if (c == 'logout') {
          t.engine.statusValue = EngineStatus(
            configured: true,
            signedIn: false,
            folder: t.syncDir,
            driveFolder: 'CloudDrop',
          );
        }
        return const ActionResult(true, 'Signed out. Uploads stop until you sign in again.');
      };

      await tester.tap(find.text('Sign out'));
      await tester.settle();
      expect(find.text('Sign out?'), findsOneWidget);
      expect(find.textContaining('Uploads stop until you sign in again.'), findsOneWidget);
      await tester.tap(find.text('Cancel'));
      await tester.settle();
      expect(t.engine.calls.where((c) => c == 'logout'), isEmpty, reason: 'cancelled');
      expect(find.text('Settings'), findsWidgets);

      await tester.tap(find.text('Sign out'));
      await tester.settle();
      await tester.tap(inDialog('Sign Out'));
      await tester.settle();
      expect(t.engine.calls.last, 'logout');
      // Straight on the assistant's sign-in page (not the welcome page), key and folder kept.
      expect(find.text('Sign in to Google'), findsOneWidget);
      expect(find.text('Sign In with Google'), findsOneWidget);
      expect(find.text('Welcome to CloudDrop'), findsNothing);
      expect(find.text('Step 5 of 6 · Sign in'), findsOneWidget);
      expect(find.text('Keep Current Sign-in'), findsNothing);

      // Signing in again goes through setup (no key re-sent) and the browser, then Done.
      await tester.tap(find.text('Sign In with Google'));
      await tester.settle();
      expect(
        t.engine.calls.firstWhere((c) => c.startsWith('setup')),
        r'setup folder=C:\Users\Dhiva\CloudDrop drive=CloudDrop json=null id=null secret=null',
      );
      t.engine.statusValue = EngineStatus(
        configured: true,
        signedIn: true,
        folder: t.syncDir,
        driveFolder: 'CloudDrop',
      );
      t.engine.logins.single.urlC.complete('https://accounts.google.com/auth?x=1');
      t.engine.logins.single.doneC.complete(const ActionResult(true, 'Signed in.'));
      await tester.settle();
      expect(find.text("You're all set"), findsOneWidget);
      await tester.tap(find.text('Finish'));
      await tester.settle();
      expect(find.text('Sign in to Google'), findsNothing);
      expect(t.controller.snapshot.signedIn, isTrue);
    });

    uiTest('a failed sign out says why and stays on Settings', (tester) async {
      final t = await settings(tester);
      t.engine.result = (c) => const ActionResult(false, 'could not stop the service');
      final msgs = <AppMessage>[];
      t.controller.messages.listen(msgs.add);
      await tester.tap(find.text('Sign out'));
      await tester.settle();
      await tester.tap(inDialog('Sign Out'));
      await tester.settle();
      expect(msgs.single.text, 'could not stop the service');
      expect(find.text('Sign in to Google'), findsNothing);
      expect(find.text('Sign out'), findsOneWidget);
    });

    uiTest('not signed in: the account card offers Sign in, no Sign out', (tester) async {
      final t = calmApp();
      t.engine.statusValue = EngineStatus(configured: true, signedIn: false, folder: t.syncDir);
      t.controller.pane.value = AppPane.settings;
      await pumpApp(tester, t);
      // (The assistant is up; leaving it for the main window shows the account card.)
      await t.controller.finishSetup();
      await tester.settle();
      expect(find.text('Not signed in'), findsOneWidget);
      expect(find.text('Sign in'), findsWidgets);
      expect(find.text('Sign out'), findsNothing);
    });

    uiTest('not signed in: the setup assistant takes over', (tester) async {
      final t = busyApp(daemon: idleDaemon());
      t.engine.statusValue = EngineStatus(configured: true, signedIn: false, folder: t.syncDir);
      t.controller.pane.value = AppPane.settings;
      await pumpApp(tester, t);
      // Not signed in: the setup assistant takes over (the sign-in page lets you keep going).
      expect(find.text('Welcome to CloudDrop'), findsOneWidget);
    });
  });
}

void otherProviders() {
  group('Other providers', () {
    uiTest('Settings › Switch Provider… asks first; the assistant can be left with Cancel', (
      tester,
    ) async {
      final t = busyApp(daemon: idleDaemon());
      t.engine.providerList = allProviders;
      t.controller.pane.value = AppPane.settings;
      await pumpApp(tester, t, size: const Size(1100, 1300));
      expect(find.text('Upload somewhere else'), findsOneWidget);
      await tapText(tester, 'Switch Provider…');
      expect(inDialog('Switch to another cloud?'), findsOneWidget);
      expect(
        find.textContaining('Nothing is deleted from Google Drive. Files that are only in'),
        findsOneWidget,
      );
      await tapText(tester, 'Cancel');
      expect(t.controller.setupActive, isFalse);

      await tapText(tester, 'Switch Provider…');
      await tapText(tester, 'Choose Cloud…');
      expect(find.text('Choose where to upload'), findsOneWidget);
      expect(find.text('Welcome to CloudDrop'), findsNothing, reason: 'no welcome when switching');
      expect(find.textContaining('Nothing is deleted from Google Drive.'), findsOneWidget);
      await tapText(tester, 'Cancel');
      expect(t.controller.setupActive, isFalse);
      expect(find.text('Upload somewhere else'), findsOneWidget, reason: 'back on Settings');
      expect(t.engine.calls, isEmpty, reason: 'nothing changed');
    });

    uiTest('switching to WebDAV: stop, setup, check, reset-cloud, then the service starts', (
      tester,
    ) async {
      final t = busyApp(daemon: idleDaemon());
      t.engine.providerList = allProviders;
      t.engine.result = (c) => c == 'check'
          ? const ActionResult(true, 'Connected to nas as me.')
          : const ActionResult(true, 'ok');
      await pumpApp(tester, t, size: const Size(1100, 1300));
      t.controller.beginSetup(switching: true);
      await tester.settle();
      await tapText(tester, 'WebDAV');
      await tapText(tester, 'Continue');
      expect(find.text('In WebDAV'), findsOneWidget);
      await tapText(tester, 'Continue');
      expect(find.text('Connect to WebDAV'), findsOneWidget);
      final boxes = find.byType(TextBox);
      await tester.enterText(boxes.at(0), 'https://nas/dav');
      await tester.enterText(boxes.at(1), 'me');
      await tester.enterText(find.byType(PasswordBox), 'pw');
      await tester.settle();
      await tapText(tester, 'Test Connection');
      expect(find.text('Connected to nas as me.'), findsOneWidget);
      await tapText(tester, 'Continue');
      expect(find.text("You're all set"), findsOneWidget);
      expect(find.textContaining('WebDAV → CloudDrop'), findsOneWidget);
      expect(t.engine.calls, [
        'service stop',
        r'setup folder=C:\Users\Dhiva\CloudDrop drive=CloudDrop json=null id=null secret=null '
            'provider=webdav --set url=https://nas/dav --set user=me --secret password=pw',
        'check',
        'reset-cloud',
        'service enable',
      ]);
    });

    uiTest('a provider without web pages: no Open in, CloudDrop holds …', (tester) async {
      final t = busyApp(
        provider: webdav,
        daemon: DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.idle),
      );
      await pumpApp(tester, t);
      expect(find.text('WebDAV'), findsWidgets);
      expect(find.byTooltip('Open in WebDAV'), findsNothing);
      expect(find.textContaining('CloudDrop holds'), findsWidgets);
      expect(find.text('Google Drive'), findsNothing);
      expect(find.textContaining('safe in Google Drive'), findsNothing);

      t.controller.showFiles(dir: 'Videos', select: 'Videos/Family reunion.mp4');
      await tester.settle();
      expect(find.byTooltip('Open in WebDAV'), findsNothing);
      await tester.tap(inList('Family reunion.mp4'), buttons: kSecondaryButton);
      await tester.settle();
      expect(inMenu('Open in WebDAV'), findsNothing);
      expect(inMenu('Show in File Explorer'), findsOneWidget);
    });

    uiTest('OneDrive: Open in OneDrive asks the engine for the link', (tester) async {
      final t = busyApp(provider: oneDrive, daemon: idleDaemon());
      t.engine.result = (c) => c.startsWith('url ')
          ? const ActionResult(true, 'https://onedrive.live.com/?id=ROOT')
          : const ActionResult(true, 'ok');
      await pumpApp(tester, t);
      expect(find.text('OneDrive'), findsWidgets);
      await tester.tap(find.byTooltip('Open in OneDrive'));
      await tester.pump();
      expect(t.system.urls, ['https://onedrive.live.com/?id=ROOT']);

      t.controller.showFiles(dir: 'Videos', select: 'Videos/Family reunion.mp4');
      await tester.settle();
      await tester.tap(find.byTooltip('Open in OneDrive'));
      await tester.pump();
      expect(t.engine.calls.last, r'url C:\Users\Dhiva\CloudDrop\Videos\Family reunion.mp4');
      expect(t.system.urls.last, 'https://onedrive.live.com/?id=ROOT');
    });

    uiTest('signed out of a form provider: Connect opens the assistant on its page', (
      tester,
    ) async {
      final t = busyApp(daemon: idleDaemon());
      t.engine.providerList = allProviders;
      t.engine.statusValue = EngineStatus(
        configured: true,
        signedIn: true,
        folder: t.syncDir,
        provider: 'webdav',
        providerName: 'WebDAV',
      );
      await pumpApp(tester, t);
      t.engine.statusValue = EngineStatus(
        configured: true,
        signedIn: false,
        folder: t.syncDir,
        provider: 'webdav',
        providerName: 'WebDAV',
      );
      await t.controller.poll(forceEngine: true);
      await tester.settle();
      expect(find.text('Connect to WebDAV'), findsOneWidget);
      await tapText(tester, 'Connect');
      expect(find.text('Test Connection'), findsOneWidget);
      expect(t.engine.logins, isEmpty, reason: 'no browser sign-in for a form');
    });
  });
}

/// The app under test and its Files controller.
class FilesCtx {
  FilesCtx(this.app, this.c);
  final TestApp app;
  final FilesController c;
}
