/// Shared plumbing for widget and golden tests: real fonts, a pinned clock, window sizes.
library;

import 'dart:convert';
import 'dart:io';

import 'package:clouddrop_app/logic/fmt.dart' as fmt;
import 'package:clouddrop_app/ui/app.dart';
import 'package:clouddrop_app/ui/scope.dart';
import 'package:fluent_ui/fluent_ui.dart' show FluentApp, ThemeMode;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fakes.dart';

/// "Now" for every UI test: Wednesday 14 Oct 2026, 14:30.
final DateTime pinnedNow = DateTime.utc(2026, 10, 14, 14, 30);
int get nowMs => pinnedNow.millisecondsSinceEpoch;
int ago({int days = 0, int hours = 0, int minutes = 0}) =>
    nowMs - ((days * 24 + hours) * 60 + minutes) * 60000;

const String testFont = 'TestSans';
const String testFallback = 'TestSansFallback';

bool _fontsLoaded = false;

/// Loads Noto Sans (from test/fonts, test only) as [testFont], and every font the app and
/// its packages declare (the Fluent icon font), so goldens show text and icons instead of boxes.
Future<void> loadTestFonts() async {
  if (_fontsLoaded) return;
  _fontsLoaded = true;
  final manifest = json.decode(await rootBundle.loadString('FontManifest.json')) as List<dynamic>;
  for (final entry in manifest) {
    final loader = FontLoader(entry['family'] as String);
    for (final font in entry['fonts'] as List<dynamic>) {
      loader.addFont(rootBundle.load(font['asset'] as String));
    }
    await loader.load();
  }
  final noto = FontLoader(testFont);
  for (final f in ['NotoSans-Regular.ttf', 'NotoSans-Bold.ttf']) {
    final bytes = File('test/fonts/$f').readAsBytesSync();
    noto.addFont(Future.value(ByteData.sublistView(bytes)));
  }
  await noto.load();
  // Arrows and symbols that Noto Sans lacks (Segoe UI has them on Windows).
  final fallback = FontLoader(testFallback);
  for (final f in ['LiberationSans-Regular.ttf', 'LiberationSans-Bold.ttf']) {
    final bytes = File('test/fonts/$f').readAsBytesSync();
    fallback.addFont(Future.value(ByteData.sublistView(bytes)));
  }
  await fallback.load();
}

/// Pins the clock and the time zone; call from `setUpAll`.
void pinTime() {
  fmt.nowProvider = () => pinnedNow;
  fmt.debugUtcTime = true;
}

void unpinTime() {
  fmt.nowProvider = DateTime.now;
  fmt.debugUtcTime = false;
}

const Size wide = Size(1100, 760);
const Size narrow = Size(480, 820);

extension WindowSize on WidgetTester {
  void useWindow(Size size) {
    view.physicalSize = size;
    view.devicePixelRatio = 1.0;
    addTearDown(view.resetPhysicalSize);
    addTearDown(view.resetDevicePixelRatio);
  }

  /// Lets controllers' futures and animations settle (no real timers are left running).
  Future<void> settle([Duration step = const Duration(milliseconds: 100), int times = 8]) async {
    for (var i = 0; i < times; i++) {
      await pump(step);
    }
  }
}

/// Pumps the whole app over [t] in a window of [size].
Future<void> pumpApp(
  WidgetTester tester,
  TestApp t, {
  Size size = wide,
  Brightness brightness = Brightness.light,
  bool settle = true,
}) async {
  tester.useWindow(size);
  await t.ready();
  await tester.pumpWidget(
    CloudDropApp(
      app: t.controller,
      fontFamily: testFont,
      fontFamilyFallback: const [testFallback],
      themeMode: brightness == Brightness.dark ? ThemeMode.dark : ThemeMode.light,
      enableDrop: false,
    ),
  );
  // Image decoding is real async work: do it up front so the logo is in the first screenshot.
  await tester.runAsync(() async {
    final ctx = tester.element(find.byType(FluentApp));
    await precacheImage(const AssetImage('assets/icon.png'), ctx);
  });
  if (settle) await tester.settle();
}

class Variant {
  const Variant(this.label, this.size, this.brightness);
  final String label;
  final Size size;
  final Brightness brightness;
}

/// Light and dark, wide (1100x760) and narrow (480 wide).
const List<Variant> variants = [
  Variant('light_wide', wide, Brightness.light),
  Variant('dark_wide', wide, Brightness.dark),
  Variant('light_narrow', narrow, Brightness.light),
  Variant('dark_narrow', narrow, Brightness.dark),
];

/// The controllers of the running app.
AppScope scopeOf(WidgetTester tester) => AppScope.of(tester.element(find.byType(MainView)));

/// A widget test that also lets toasts (InfoBars) time out, so no timer outlives the test.
void uiTest(String description, Future<void> Function(WidgetTester tester) body) {
  testWidgets(description, (tester) async {
    await body(tester);
    await tester.pump(const Duration(seconds: 12));
    await tester.pump(const Duration(seconds: 1));
  });
}
