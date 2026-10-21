/// Native window setup: size, title and the Windows 11 Mica material.
library;

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_acrylic/flutter_acrylic.dart';
import 'package:window_manager/window_manager.dart';

bool get _desktop => !kIsWeb && (Platform.isWindows || Platform.isLinux);

/// Windows 11 starts at build 22000; Mica doesn't exist before that.
bool get _windows11 {
  if (kIsWeb || !Platform.isWindows) return false;
  final m = RegExp(r'Build (\d+)').firstMatch(Platform.operatingSystemVersion);
  final build = m == null ? 0 : int.tryParse(m.group(1)!) ?? 0;
  return build >= 22000;
}

/// Sets the window up. Returns true when the Mica material is active (Windows 11), so the
/// pages draw transparent backgrounds over it.
Future<bool> initWindow() async {
  if (!_desktop) return false;
  var mica = false;
  try {
    if (_windows11) {
      await Window.initialize();
      await Window.setEffect(effect: WindowEffect.mica, dark: false);
      mica = true;
    }
  } catch (e) {
    debugPrint('Mica is not available: $e');
    mica = false;
  }
  try {
    await windowManager.ensureInitialized();
    const options = WindowOptions(
      size: Size(1000, 760),
      minimumSize: Size(420, 520),
      center: true,
      title: 'CloudDrop',
    );
    await windowManager.waitUntilReadyToShow(options, () async {
      await windowManager.show();
      await windowManager.focus();
    });
  } catch (e) {
    debugPrint('Window setup failed: $e');
  }
  return mica;
}

/// The window's title ("Work - CloudDrop" with several folders).
Future<void> setWindowTitle(String title) async {
  if (!_desktop) return;
  try {
    await windowManager.setTitle(title);
  } catch (e) {
    debugPrint('Window title update failed: $e');
  }
}

/// Makes the Mica material follow the app's light/dark theme.
Future<void> syncMica(Brightness brightness) async {
  if (!_windows11) return;
  try {
    await Window.setEffect(effect: WindowEffect.mica, dark: brightness == Brightness.dark);
  } catch (e) {
    debugPrint('Mica update failed: $e');
  }
}
