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
  AccentColor? accent,
}) {
  var base = FluentThemeData(
    brightness: brightness,
    fontFamily: fontFamily,
    accentColor: accent ?? brandAccent,
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

/// CloudDrop blue, the middle of the logo's sky gradient, as a Fluent accent.
final AccentColor brandAccent = AccentColor.swatch(const <String, Color>{
  'darkest': Color(0xFF0E3A8F),
  'darker': Color(0xFF1650BA),
  'dark': Color(0xFF1F63D6),
  'normal': Color(0xFF2F7CF0),
  'light': Color(0xFF5A98F4),
  'lighter': Color(0xFF84B3F7),
  'lightest': Color(0xFFAECEFA),
});

/// The `accent` setting's colours, as offered in Settings (`gui/src/ui.rs` has the same ones).
/// "default" is CloudDrop blue, so "blue" isn't offered separately here.
const Map<String, Color> accentChoices = {
  'default': Color(0xFF2F7CF0),
  'teal': Color(0xFF0F8B8D),
  'green': Color(0xFF3A944A),
  'purple': Color(0xFF7C4DD6),
  'pink': Color(0xFFD0457F),
  'orange': Color(0xFFE2620E),
};

/// The accent for the `accent` setting; unknown names get CloudDrop blue.
AccentColor accentFor(String name) {
  final base = accentChoices[name];
  if (base == null || name == 'default') return brandAccent;
  Color shade(Color to, double t) => Color.lerp(base, to, t)!;
  return AccentColor.swatch({
    'darkest': shade(Colors.black, 0.45),
    'darker': shade(Colors.black, 0.3),
    'dark': shade(Colors.black, 0.15),
    'normal': base,
    'light': shade(Colors.white, 0.2),
    'lighter': shade(Colors.white, 0.4),
    'lightest': shade(Colors.white, 0.6),
  });
}

/// The logo's colours, for the few places that wear the brand (Home's status card, setup).
abstract final class Brand {
  static const Color cyan = Color(0xFF41D9EF);
  static const Color blue = Color(0xFF2F7CF0);
  static const Color indigo = Color(0xFF4B3FD8);

  /// A soft wash of the logo's sky behind a card.
  static Gradient wash(Brightness b) => LinearGradient(
    begin: Alignment.topLeft,
    end: Alignment.bottomRight,
    colors: b == Brightness.dark
        ? const [Color(0xFF0F2A3D), Color(0xFF15254A), Color(0xFF211B4A)]
        : const [Color(0xFFE6F8FD), Color(0xFFE9F0FE), Color(0xFFEEECFD)],
  );

  /// The logo's sky itself.
  static const Gradient sky = LinearGradient(
    begin: Alignment.topLeft,
    end: Alignment.bottomRight,
    colors: [cyan, blue, indigo],
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
