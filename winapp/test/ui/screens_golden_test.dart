/// Golden screenshots of every main screen: light and dark, wide (1100x760) and narrow
/// (480 wide). Regenerate with `flutter test --update-goldens test/ui`, then look at the
/// PNGs in test/goldens/.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';
import '../support/harness.dart';
import '../support/scenarios.dart';

typedef Act = Future<void> Function(WidgetTester tester, TestApp t);

void screen(
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
      // [tall] shows a whole scrolling page: a window as high as the content.
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

void main() {
  setUpAll(() async {
    pinTime();
    await loadTestFonts();
  });
  tearDownAll(unpinTime);

  // ---------- the settings file can't be read ----------
  screen('config_error', () {
    final t = TestApp(
      windows: true,
      status: const EngineStatus.unconfigured(
        configError:
            r'invalid config C:\Users\Dhiva\AppData\Roaming\clouddrop\config.toml: TOML parse error at line 4, column 12'
            '\n  |\n4 | sync_dir = C:\\Users\\Dhiva\\CloudDrop\n  |            ^\ninvalid escape\n',
      ),
    );
    t.engine.config = null;
    return t;
  });

  // ---------- Home ----------
  screen('home_busy', busyApp, act: (tester, t) => t.measureSpeed(3 * mb));
  screen('home_idle', calmApp);
  screen('home_paused', () {
    final t = busyApp(
      daemon: DaemonStatus(
        pid: 1,
        updatedMs: nowMs,
        phase: Phase.paused,
        waiting: 3,
        quota: const Quota(limit: 15 * gb, usage: 14 * gb + 400 * mb, usageInDrive: 3 * gb),
      ),
    );
    t.db.meta('paused', '1');
    return t;
  });
  screen('home_not_running', () {
    final t = calmApp();
    t.statusSource.value = null;
    return t;
  });
  screen(
    'home_offline',
    () => busyApp(
      daemon: DaemonStatus(pid: 1, updatedMs: nowMs, phase: Phase.offline, waiting: 4),
    ),
  );

  // ---------- Files ----------
  screen('files_root', busyApp, pane: AppPane.files);
  screen(
    'files_folder',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      scopeOf(tester).files.openDir('Documents/Invoices');
      await tester.settle();
    },
  );
  screen(
    'files_selection',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      final files = scopeOf(tester).files..openDir('Photos');
      await tester.settle();
      files.selectOnly('Photos/Goa trip');
    },
  );
  screen(
    'files_multi_selection',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      final files = scopeOf(tester).files;
      files
        ..selectOnly('Quarterly report.pdf')
        ..toggle('notes.txt')
        ..toggle('Videos');
    },
  );
  screen(
    'files_search',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      scopeOf(tester).files.searchChanged('inv');
    },
  );
  screen(
    'files_failed',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      scopeOf(tester).files.setFilter(Filter.failed);
    },
  );
  screen(
    'files_cloud_only',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      scopeOf(tester).files.setFilter(Filter.cloudOnly);
    },
  );
  // Windows: a freed file still on disk as a placeholder opens (Open leads the command bar).
  screen(
    'files_placeholder',
    () {
      final t = busyApp(cloudFiles: true);
      t.onDisk.add(r'C:\Users\Dhiva\CloudDrop\Videos\Family reunion.mp4');
      return t;
    },
    pane: AppPane.files,
    act: (tester, t) async {
      final files = scopeOf(tester).files..openDir('Videos');
      await tester.settle();
      files.selectOnly('Videos/Family reunion.mp4');
    },
  );
  screen('files_empty', () {
    final t = TestApp(windows: true, daemon: idleDaemon(), config: busyConfig);
    return t;
  }, pane: AppPane.files);
  screen(
    'files_no_results',
    busyApp,
    pane: AppPane.files,
    act: (tester, t) async {
      scopeOf(tester).files.searchChanged('zzzz');
    },
  );

  // ---------- Activity ----------
  screen('activity', busyApp, pane: AppPane.activity);
  screen(
    'activity_empty',
    () => TestApp(windows: true, daemon: idleDaemon(), config: busyConfig),
    pane: AppPane.activity,
  );

  // ---------- Settings ----------
  screen('settings', busyApp, pane: AppPane.settings, tall: 1250);
  screen(
    'settings_sign_out_dialog',
    busyApp,
    pane: AppPane.settings,
    tall: 1250,
    act: (tester, t) async {
      await tester.tap(find.text('Sign out'));
    },
  );
  screen(
    'settings_custom',
    () {
      // A custom number of days, nothing pinned, the engine not running.
      final t = calmApp();
      t.engine.config = busyConfig.copyWith(autoFreeDays: 14, keepLocal: const []);
      t.statusSource.value = null;
      t.engine.service = const ServiceStatus(running: false, startsAtSignIn: false);
      return t;
    },
    pane: AppPane.settings,
    tall: 1100,
  );
}
