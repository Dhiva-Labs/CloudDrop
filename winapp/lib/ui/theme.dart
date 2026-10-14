/// Windows 11 Fluent look: theme data and the few colours the screens share.
library;

import 'package:fluent_ui/fluent_ui.dart';

/// The app theme. [mica] makes the page backgrounds transparent so the window's Mica
/// material shows through (Windows 11 only; other systems get solid backgrounds).
FluentThemeData buildTheme(
  Brightness brightness, {
  bool mica = false,
  String? fontFamily,
  List<String>? fontFamilyFallback,
}) {
  var base = FluentThemeData(
    brightness: brightness,
    fontFamily: fontFamily,
    accentColor: Colors.blue,
    visualDensity: VisualDensity.standard,
  );
  if (fontFamilyFallback != null) {
    // Glyphs the main font lacks (arrows, symbols) come from these (used by tests).
    final t = base.typography;
    TextStyle? f(TextStyle? s) => s?.copyWith(fontFamilyFallback: fontFamilyFallback);
    base = base.copyWith(
      typography: Typography.raw(
        display: f(t.display),
        titleLarge: f(t.titleLarge),
        title: f(t.title),
        subtitle: f(t.subtitle),
        bodyLarge: f(t.bodyLarge),
        bodyStrong: f(t.bodyStrong),
        body: f(t.body),
        caption: f(t.caption),
      ),
    );
  }
  if (!mica) return base;
  return base.copyWith(
    scaffoldBackgroundColor: Colors.transparent,
    micaBackgroundColor: Colors.transparent,
  );
}

/// Pairs of (foreground, background) for status chips and icon badges.
class Tint {
  const Tint(this.fg, this.bg);
  final Color fg;
  final Color bg;
}

extension AppColors on FluentThemeData {
  bool get isDark => brightness == Brightness.dark;

  Tint get success =>
      Tint(resources.systemFillColorSuccess, resources.systemFillColorSuccessBackground);
  Tint get caution =>
      Tint(resources.systemFillColorCaution, resources.systemFillColorCautionBackground);
  Tint get critical =>
      Tint(resources.systemFillColorCritical, resources.systemFillColorCriticalBackground);
  Tint get neutral =>
      Tint(resources.textFillColorSecondary, resources.systemFillColorNeutralBackground);
  Tint get accent {
    final c = accentColor.defaultBrushFor(brightness);
    return Tint(c, c.withValues(alpha: isDark ? 0.22 : 0.14));
  }

  Color get secondaryText => resources.textFillColorSecondary;
  Color get cardFill => resources.cardBackgroundFillColorDefault;
  Color get cardStroke => resources.cardStrokeColorDefault;
}

/// Side margin of page contents: generous on wide windows, tight on narrow ones
/// (the same switch point as Fluent's `PageHeader`).
double pageGutter(double width) => width < 640 ? 14 : 24;

/// Standard page padding.
EdgeInsets pagePadding(double width) {
  final h = pageGutter(width);
  return EdgeInsets.fromLTRB(h, 4, h, 24);
}
