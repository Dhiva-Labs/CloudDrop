/// The window: Fluent app, navigation (Home / Files / Activity / Settings), the setup
/// assistant before that, and toasts.
library;

import 'dart:async';

import 'package:fluent_ui/fluent_ui.dart';

import '../state/activity_controller.dart';
import '../state/app_controller.dart';
import '../state/files_controller.dart';
import '../logic/snapshot.dart';
import '../state/setup_controller.dart';
import 'pages/activity_page.dart';
import 'pages/files_page.dart';
import 'pages/home_page.dart';
import 'pages/settings_page.dart';
import 'scope.dart';
import 'setup/setup_page.dart';
import 'theme.dart';
import 'widgets/connection_switcher.dart';

/// The window's title: "CloudDrop", or with several folders the one shown ("Work - CloudDrop").
String windowTitle(AppController app) =>
    app.several && app.label != 'CloudDrop' ? '${app.label} - CloudDrop' : 'CloudDrop';

class CloudDropApp extends StatelessWidget {
  const CloudDropApp({
    super.key,
    required this.app,
    this.mica = false,
    this.fontFamily,
    this.fontFamilyFallback,
    this.themeMode = ThemeMode.system,
    this.enableDrop = true,
    this.onBrightness,
    this.onTitle,
  });

  final AppController app;

  /// The window has the Mica material behind it (Windows 11).
  final bool mica;
  final String? fontFamily;
  final List<String>? fontFamilyFallback;
  final ThemeMode themeMode;

  /// Off in tests: the native drop plugin isn't there.
  final bool enableDrop;

  /// Called when the effective brightness changes (the window effect follows it).
  final void Function(Brightness brightness)? onBrightness;

  /// Called when the window's title changes ([windowTitle]).
  final void Function(String title)? onTitle;

  @override
  Widget build(BuildContext context) {
    return FluentApp(
      title: 'CloudDrop',
      debugShowCheckedModeBanner: false,
      themeMode: themeMode,
      theme: buildTheme(
        Brightness.light,
        mica: mica,
        fontFamily: fontFamily,
        fontFamilyFallback: fontFamilyFallback,
      ),
      darkTheme: buildTheme(
        Brightness.dark,
        mica: mica,
        fontFamily: fontFamily,
        fontFamilyFallback: fontFamilyFallback,
      ),
      home: _Root(app: app, enableDrop: enableDrop, onBrightness: onBrightness, onTitle: onTitle),
    );
  }
}

class _Root extends StatefulWidget {
  const _Root({required this.app, required this.enableDrop, this.onBrightness, this.onTitle});

  final AppController app;
  final bool enableDrop;
  final void Function(Brightness brightness)? onBrightness;
  final void Function(String title)? onTitle;

  @override
  State<_Root> createState() => _RootState();
}

class _RootState extends State<_Root> {
  late final FilesController _files = FilesController(widget.app);
  late final ActivityController _activity = ActivityController(widget.app);
  StreamSubscription<AppMessage>? _messages;
  Brightness? _reported;
  String _title = 'CloudDrop';

  @override
  void initState() {
    super.initState();
    _messages = widget.app.messages.listen(_toast);
    widget.app.addListener(_retitle);
  }

  void _retitle() {
    final t = windowTitle(widget.app);
    if (t == _title) return;
    _title = t;
    widget.onTitle?.call(t);
  }

  @override
  void dispose() {
    widget.app.removeListener(_retitle);
    _messages?.cancel();
    _files.dispose();
    _activity.dispose();
    super.dispose();
  }

  void _toast(AppMessage m) {
    if (!mounted) return;
    final severity = switch (m.severity) {
      Severity.info => InfoBarSeverity.info,
      Severity.success => InfoBarSeverity.success,
      Severity.warning => InfoBarSeverity.warning,
      Severity.error => InfoBarSeverity.error,
    };
    displayInfoBar(
      context,
      duration: Duration(seconds: m.actionLabel == null ? 5 : 8),
      alignment: Alignment.bottomCenter,
      builder: (context, close) => ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560),
        child: InfoBar(
          title: Text(m.text),
          severity: severity,
          isLong: m.text.length > 70,
          action: m.actionLabel == null
              ? null
              : Button(
                  onPressed: () {
                    close();
                    m.onAction?.call();
                  },
                  child: Text(m.actionLabel!),
                ),
          onClose: close,
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final brightness = FluentTheme.of(context).brightness;
    if (_reported != brightness) {
      _reported = brightness;
      widget.onBrightness?.call(brightness);
    }
    return AppScope(
      app: widget.app,
      files: _files,
      activity: _activity,
      child: _Gate(app: widget.app, enableDrop: widget.enableDrop),
    );
  }
}

/// Splash -> engine problem -> setup assistant -> main window.
class _Gate extends StatefulWidget {
  const _Gate({required this.app, required this.enableDrop});

  final AppController app;
  final bool enableDrop;

  @override
  State<_Gate> createState() => _GateState();
}

class _GateState extends State<_Gate> {
  SetupController? _setup;
  bool _making = false;

  Future<void> _ensureSetup() async {
    if (_setup != null || _making) return;
    _making = true;
    final app = widget.app;
    final adding = app.setupAdding;
    // A new folder has no database yet; the one shown now keeps its folder.
    // (Always asynchronous: this runs from build.)
    final locked = await (adding ? Future<String?>.value() : app.db.meta('sync_root'));
    if (!mounted) return;
    final atSignIn = app.setupAtSignIn;
    final switching = app.setupSwitching;
    app.setupAtSignIn = false;
    app.setupSwitching = false;
    app.setupAdding = false;
    setState(() {
      _setup = SetupController(
        app,
        lockedRoot: locked,
        startAtSignIn: atSignIn,
        switching: switching,
        adding: adding,
      );
      _making = false;
    });
  }

  @override
  void dispose() {
    _setup?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final app = widget.app;
    return ListenableBuilder(
      listenable: app,
      builder: (context, _) {
        if (!app.loaded) return const _Splash();
        if (app.fatal != null) return _Problem(app: app);
        if (app.configError != null) return _ConfigProblem(app: app);
        if (app.setupActive) {
          _ensureSetup();
          final s = _setup;
          return s == null ? const _Splash() : SetupPage(controller: s);
        }
        if (_setup != null) {
          final old = _setup!;
          _setup = null;
          WidgetsBinding.instance.addPostFrameCallback((_) => old.dispose());
        }
        return MainView(app: app, enableDrop: widget.enableDrop);
      },
    );
  }
}

class _Splash extends StatelessWidget {
  const _Splash();

  @override
  Widget build(BuildContext context) => const ScaffoldPage(content: Center(child: ProgressRing()));
}

/// The engine (`clouddrop`) can't be run.
class _Problem extends StatelessWidget {
  const _Problem({required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return ScaffoldPage(
      content: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  app.fatalIsMissingCli
                      ? "CloudDrop can't start its engine"
                      : "CloudDrop can't read its state",
                  style: theme.typography.subtitle,
                ),
                const SizedBox(height: 12),
                InfoBar(
                  title: Text(app.fatal ?? ''),
                  content: Text(
                    app.fatalIsMissingCli
                        ? 'The app needs clouddrop.exe next to it (or on PATH, or CLOUDDROP_BIN pointing at it).'
                        : 'Settings file: ${app.paths.configPath}',
                  ),
                  severity: InfoBarSeverity.error,
                  isLong: true,
                ),
                const SizedBox(height: 16),
                Align(
                  alignment: Alignment.centerLeft,
                  child: FilledButton(
                    onPressed: () => app.refreshNow(),
                    child: const Text('Try Again'),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The settings file exists but can't be read. Nothing else runs: the setup assistant would
/// overwrite the user's file.
class _ConfigProblem extends StatelessWidget {
  const _ConfigProblem({required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final error = app.configError ?? '';
    final firstLine = error.trim().split(RegExp(r'\r?\n')).first;
    final path = app.configPath;
    return ScaffoldPage(
      content: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 560),
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                // With several folders the others stay a click away.
                if (app.several) ...[
                  Align(
                    alignment: AlignmentDirectional.centerStart,
                    child: ConnectionMenu(app: app),
                  ),
                  const SizedBox(height: 16),
                ],
                Text("CloudDrop's settings can't be read", style: theme.typography.subtitle),
                const SizedBox(height: 12),
                InfoBar(
                  title: Text(firstLine),
                  // The engine's sentence usually names the file already.
                  content: Text(
                    firstLine.contains(path)
                        ? 'Fix the file or move it away, then try again. Nothing was changed.'
                        : 'Settings file: $path\nFix the file or move it away, then try again.',
                  ),
                  severity: InfoBarSeverity.error,
                  isLong: true,
                ),
                const SizedBox(height: 16),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    FilledButton(onPressed: () => app.refreshNow(), child: const Text('Try Again')),
                    Button(
                      onPressed: app.openSettingsFolder,
                      child: const Text('Open Settings Folder'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Navigation view with the four screens.
class MainView extends StatelessWidget {
  const MainView({super.key, required this.app, this.enableDrop = true});

  final AppController app;
  final bool enableDrop;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([app.pane, app]),
      builder: (context, _) {
        final selected = app.pane.value.index;
        final failed = app.snapshot.counts.failed;
        // Wide: the full pane; medium: icons only; narrow: tabs along the top (always
        // visible, instead of a hidden hamburger menu).
        final width = MediaQuery.widthOf(context);
        final mode = width >= 1000
            ? PaneDisplayMode.expanded
            : width >= 640
            ? PaneDisplayMode.compact
            : PaneDisplayMode.top;
        return NavigationView(
          pane: NavigationPane(
            selected: selected,
            onChanged: (i) => app.pane.value = AppPane.values[i],
            displayMode: mode,
            size: const NavigationPaneSize(openWidth: 232, openMinWidth: 232),
            header: mode == PaneDisplayMode.top
                ? null
                : Padding(
                    padding: const EdgeInsetsDirectional.fromSTEB(14, 6, 12, 10),
                    child: Row(
                      children: [
                        Image.asset(
                          'assets/icon.png',
                          width: 22,
                          height: 22,
                          filterQuality: FilterQuality.medium,
                          errorBuilder: (_, _, _) => const SizedBox(width: 22, height: 22),
                        ),
                        const SizedBox(width: 10),
                        Text('CloudDrop', style: FluentTheme.of(context).typography.bodyStrong),
                      ],
                    ),
                  ),
            // The status, on every screen (the pane's search slot, when the pane is open).
            autoSuggestBox: mode == PaneDisplayMode.expanded ? _StatusChip(app: app) : null,
            items: [
              // Several folders: each one with its status, above the screens.
              if (mode != PaneDisplayMode.top)
                ...connectionPaneItems(context, app, compact: mode == PaneDisplayMode.compact),
              PaneItem(
                icon: const Icon(FluentIcons.home),
                title: const Text('Home'),
                body: const SizedBox.shrink(),
              ),
              PaneItem(
                icon: const Icon(FluentIcons.fabric_folder),
                title: const Text('Files'),
                infoBadge: failed > 0
                    ? InfoBadge(source: Text('$failed'), severity: InfoBarSeverity.error)
                    : null,
                body: const SizedBox.shrink(),
              ),
              PaneItem(
                icon: const Icon(FluentIcons.history),
                title: const Text('Activity'),
                body: const SizedBox.shrink(),
              ),
            ],
            footerItems: [
              PaneItem(
                icon: const Icon(FluentIcons.settings),
                title: const Text('Settings'),
                body: const SizedBox.shrink(),
              ),
            ],
          ),
          paneBodyBuilder: (item, body) {
            final pages = IndexedStack(
              index: selected,
              children: [
                const HomePage(),
                FilesPage(enableDrop: enableDrop),
                const ActivityPage(),
                const SettingsPage(),
              ],
            );
            if (mode != PaneDisplayMode.top || !app.several) return pages;
            // Narrow, several folders: the switcher above the screen (the tabs keep the top).
            final gutter = pageGutter(width);
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Padding(
                  padding: EdgeInsets.fromLTRB(gutter, 12, gutter, 0),
                  child: Align(
                    alignment: AlignmentDirectional.centerStart,
                    child: ConnectionMenu(app: app),
                  ),
                ),
                Expanded(child: pages),
              ],
            );
          },
        );
      },
    );
  }
}

/// "● Up to date" under the pane header; opens Home.
class _StatusChip extends StatelessWidget {
  const _StatusChip({required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final sum = summarize(app.snapshot);
    final dot = statusDotColor(theme, sum.icon);
    return Tooltip(
      message: sum.title,
      child: HoverButton(
        onPressed: () => app.pane.value = AppPane.home,
        builder: (context, states) => Container(
          height: 30,
          padding: const EdgeInsets.symmetric(horizontal: 10),
          decoration: BoxDecoration(
            color: states.isHovered
                ? theme.resources.subtleFillColorTertiary
                : theme.resources.subtleFillColorSecondary,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Row(
            children: [
              Container(
                width: 8,
                height: 8,
                decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  sum.line,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.typography.caption,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
