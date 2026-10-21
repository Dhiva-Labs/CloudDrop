/// Widget tests of several folders: the switcher (pane and drop-down), "Add Another Folder…"
/// and "Remove This Folder…", driven like a user would, against fakes.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/ui/app.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';
import '../support/scenarios.dart';

Future<void> tapText(WidgetTester tester, String text, {int index = 0}) async {
  final f = find.text(text).at(index);
  await tester.ensureVisible(f);
  await tester.tap(f);
  await tester.settle();
}

Finder inDialog(String text) =>
    find.descendant(of: find.byType(ContentDialog), matching: find.text(text));

Finder inMenu(String text) =>
    find.descendant(of: find.byType(MenuFlyout), matching: find.text(text));

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  uiTest('one folder: no switcher, Settings still offers to add one', (tester) async {
    final t = calmApp();
    await pumpApp(tester, t);
    expect(find.text('Add Another Folder…'), findsNothing);
    expect(find.text('Work'), findsNothing);
    t.controller.pane.value = AppPane.settings;
    await tester.settle();
    expect(find.text('Add Another Folder…'), findsOneWidget);
    expect(find.text('Remove This Folder…'), findsOneWidget);
  });

  uiTest('the pane lists the folders; a click shows the other one and retitles the window', (
    tester,
  ) async {
    final t = twoFoldersApp();
    final titles = <String>[];
    tester.useWindow(wide);
    await t.ready();
    await tester.pumpWidget(
      CloudDropApp(
        app: t.controller,
        fontFamily: testFont,
        fontFamilyFallback: const [testFallback],
        themeMode: ThemeMode.light,
        enableDrop: false,
        onTitle: titles.add,
      ),
    );
    await tester.settle();
    expect(find.text('CloudDrop'), findsWidgets);
    expect(find.text('Work'), findsOneWidget);
    expect(find.text('Everything is uploaded'), findsOneWidget);
    expect(find.bySemanticsLabel(RegExp(r'^Work · Uploading')), findsOneWidget);

    await tapText(tester, 'Work');
    expect(t.controller.connectionName, 'Work');
    expect(t.prefs.remembered, 'Work');
    expect(find.text('Uploading 2 files'), findsOneWidget);
    expect(find.text('OneDrive'), findsWidgets);
    expect(titles.last, 'Work - CloudDrop');

    // Files shows Work's top, named after it.
    t.controller.pane.value = AppPane.files;
    await tester.settle();
    expect(find.text('Contracts.pdf'), findsOneWidget);
    expect(find.text('Budget 2026.xlsx'), findsNothing);

    // And back.
    await tapText(tester, 'CloudDrop', index: 1);
    expect(t.controller.connectionName, 'default');
    expect(titles.last, 'CloudDrop');
    await tester.settle();
    expect(find.text('Documents'), findsOneWidget);
  });

  uiTest('narrow window: a drop-down above the screen', (tester) async {
    final t = twoFoldersApp();
    await pumpApp(tester, t, size: narrow);
    await tester.tap(find.byType(DropDownButton));
    await tester.settle();
    expect(inMenu('Work'), findsOneWidget);
    expect(inMenu('Add Another Folder…'), findsOneWidget);
    await tester.tap(inMenu('Work'));
    await tester.settle();
    expect(t.controller.connectionName, 'Work');
    expect(find.text('Uploading 2 files'), findsOneWidget);

    await tester.tap(find.byType(DropDownButton));
    await tester.settle();
    await tester.tap(inMenu('Add Another Folder…'));
    await tester.settle();
    expect(find.text('Add Another Folder'), findsOneWidget);
  });

  uiTest('Add Another Folder…: the assistant, and Cancel goes back to the folder shown', (
    tester,
  ) async {
    final t = twoFoldersApp(remember: 'Work');
    await pumpApp(tester, t);
    await tapText(tester, 'Add Another Folder…');
    expect(find.text('Add Another Folder'), findsOneWidget);
    expect(find.text('Choose where to upload'), findsOneWidget);
    expect(find.text('Welcome to CloudDrop'), findsNothing);
    await tapText(tester, 'Continue');
    expect(find.text('No folder chosen yet'), findsOneWidget);
    await tapText(tester, 'Cancel');
    expect(find.text('Choose where to upload'), findsNothing);
    expect(t.controller.connectionName, 'Work');
    expect(find.text('Uploading 2 files'), findsOneWidget);
    expect(t.engine.log, isEmpty, reason: 'nothing was written');
  });

  uiTest('Add Another Folder…: a whole run to the new folder', (tester) async {
    final t = twoFoldersApp();
    t.system.directory = r'C:\Users\Dhiva\Pictures';
    await pumpApp(tester, t);
    t.controller.pane.value = AppPane.settings;
    await tester.settle();
    await tapText(tester, 'Add Another Folder…');
    await tapText(tester, 'Continue');
    await tapText(tester, 'Choose…');
    expect(find.textContaining('CloudDrop calls it “Pictures”'), findsOneWidget);
    await tapText(tester, 'Continue');
    await tapText(tester, 'Sign In with Google');
    expect(
      t.engine.log.single,
      r'default: connections add Pictures folder=C:\Users\Dhiva\Pictures drive=Pictures '
      'provider=gdrive',
    );
    t.engine.view('Pictures').logins.single
      ..urlC.complete('https://accounts.example/auth')
      ..doneC.complete(const ActionResult(true, 'Signed in.'));
    await tester.settle();
    expect(find.text("You're all set"), findsOneWidget);
    await tapText(tester, 'Finish');
    expect(t.controller.connectionName, 'Pictures');
    expect(t.prefs.remembered, 'Pictures');
    expect(find.text('Pictures'), findsWidgets);
  });

  uiTest('Remove This Folder…: asks first; Remove Folder removes it and shows the other', (
    tester,
  ) async {
    final t = twoFoldersApp(remember: 'Work');
    await pumpApp(tester, t, size: const Size(1100, 1400));
    t.controller.pane.value = AppPane.settings;
    await tester.settle();
    await tester.tap(find.text('Remove This Folder…'));
    await tester.settle();
    expect(inDialog('Remove this folder?'), findsOneWidget);
    expect(find.textContaining(r'stops uploading C:\Users\Dhiva\Work'), findsOneWidget);
    expect(find.textContaining('OneDrive keeps everything'), findsOneWidget);
    await tester.tap(inDialog('Cancel'));
    await tester.settle();
    expect(t.engine.log, isEmpty);

    await tester.tap(find.text('Remove This Folder…'));
    await tester.settle();
    await tester.tap(inDialog('Remove Folder'));
    await tester.settle();
    expect(t.engine.log, ['default: connections remove Work']);
    expect(t.controller.connectionName, 'default');
    expect(t.controller.switcher, isEmpty, reason: 'one folder left: no switcher');
    expect(find.text(r'C:\Users\Dhiva\CloudDrop'), findsOneWidget, reason: 'its Settings');
    expect(find.textContaining('no longer uploads “Work”'), findsOneWidget);
  });

  uiTest("a folder whose settings can't be read still lets you switch to the others", (
    tester,
  ) async {
    final t = twoFoldersApp(remember: 'Work');
    t.engine.view('Work').statusValue = const EngineStatus.unconfigured(
      configError: 'invalid config: TOML parse error at line 1, column 5',
    );
    await pumpApp(tester, t);
    expect(find.text("CloudDrop's settings can't be read"), findsOneWidget);
    expect(
      find.textContaining('/connections/Work/config.toml'),
      findsNothing,
      reason: 'Windows paths in this scenario',
    );
    expect(find.textContaining(r'connections\Work\config.toml'), findsOneWidget);
    await tester.tap(find.byType(DropDownButton));
    await tester.settle();
    await tester.tap(inMenu('CloudDrop'));
    await tester.settle();
    expect(t.controller.connectionName, 'default');
    expect(find.text('Everything is uploaded'), findsOneWidget);
  });

  uiTest('removing the only folder leads to the setup assistant', (tester) async {
    final t = calmApp();
    await pumpApp(tester, t, size: const Size(1100, 1400));
    t.controller.pane.value = AppPane.settings;
    await tester.settle();
    await tester.tap(find.text('Remove This Folder…'));
    await tester.settle();
    expect(find.textContaining('your only folder'), findsOneWidget);
    await tester.tap(inDialog('Remove Folder'));
    await tester.settle();
    expect(t.engine.log, ['default: connections remove default']);
    expect(find.text('Welcome to CloudDrop'), findsOneWidget);
  });
}
