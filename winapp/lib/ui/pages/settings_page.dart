/// Settings: space saving, account, folders, uploading, background
/// (`gui/src/settings.rs`). Every change is a `clouddrop config set` / service command and
/// takes effect at once.
library;

import 'dart:async';

import 'package:fluent_ui/fluent_ui.dart';

import '../../data/engine_api.dart';
import '../../data/fs_ops.dart';
import '../../data/models.dart';
import '../../logic/settings_logic.dart';
import '../../state/app_controller.dart';
import '../scope.dart';
import '../theme.dart';
import '../widgets/common.dart';
import '../widgets/dialogs.dart';
import '../widgets/page_header.dart';
import '../widgets/sign_in_dialog.dart';

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key});

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  final _ignore = TextEditingController();
  final _ignoreFocus = FocusNode();
  AppController? _app;
  String _shownIgnore = '';
  Timer? _parallelTimer;
  Timer? _settleTimer;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final app = AppScope.of(context).app;
    if (!identical(app, _app)) {
      _app?.removeListener(_sync);
      _app = app..addListener(_sync);
      _sync();
      // The "starts at sign-in" switch is read from the engine.
      app.refreshService();
    }
  }

  /// Keeps the ignore box in step with the config unless the user is editing it.
  void _sync() {
    final cfg = _app?.config;
    if (cfg == null) return;
    final text = commaList(cfg.ignore);
    if (text != _shownIgnore && !_ignoreFocus.hasFocus) {
      _shownIgnore = text;
      _ignore.text = text;
    }
  }

  @override
  void dispose() {
    _app?.removeListener(_sync);
    _parallelTimer?.cancel();
    _settleTimer?.cancel();
    _ignore.dispose();
    _ignoreFocus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final app = AppScope.of(context).app;
    return ScaffoldPage(
      padding: const EdgeInsets.only(top: 14),
      header: const AppPageHeader('Settings'),
      content: ListenableBuilder(
        listenable: app,
        builder: (context, _) {
          final cfg = app.config;
          if (cfg == null) {
            return const Center(child: Text('CloudDrop is not set up yet.'));
          }
          return PageBody(
            children: [
              const SectionTitle('Space saving', padding: EdgeInsets.only(top: 4, bottom: 8)),
              _autoFree(context, app, cfg),
              const SizedBox(height: 4),
              _keepFolders(context, app, cfg),
              const SectionTitle('Google account'),
              _account(context, app),
              const SectionTitle('Folders'),
              _SettingCard(
                icon: FluentIcons.pc1,
                title: 'On this computer',
                subtitle: app.paths.pretty(cfg.syncDir),
                trailing: Tooltip(
                  message: 'Open folder',
                  child: IconButton(
                    icon: const Icon(FluentIcons.open_folder_horizontal, size: 14),
                    onPressed: app.openFolder,
                  ),
                ),
              ),
              const SizedBox(height: 4),
              _SettingCard(
                icon: FluentIcons.cloud,
                title: 'In Google Drive',
                subtitle: 'My Drive → ${cfg.driveFolder}',
                trailing: Tooltip(
                  message: 'Open in Google Drive',
                  child: IconButton(
                    icon: const Icon(FluentIcons.open_in_new_window, size: 14),
                    onPressed: app.openRootInDrive,
                  ),
                ),
              ),
              const SectionTitle('Uploading'),
              _uploading(context, app, cfg),
              const SectionTitle('Background'),
              _background(context, app),
            ],
          );
        },
      ),
    );
  }

  // ---------- space saving ----------

  Widget _autoFree(BuildContext context, AppController app, AppConfig cfg) {
    final options = autoFreeOptions(cfg.autoFreeDays);
    final index = options.indexOf(cfg.autoFreeDays);
    return _SettingCard(
      icon: FluentIcons.hard_drive,
      title: 'Free up space automatically',
      stack: true,
      subtitle:
          'Local copies are removed only after CloudDrop checks Google Drive has an identical copy',
      trailing: SizedBox(
        width: 190,
        child: ComboBox<int>(
          isExpanded: true,
          value: index < 0 ? 0 : index,
          items: [
            for (var i = 0; i < options.length; i++)
              ComboBoxItem<int>(value: i, child: Text(autoFreeLabel(options[i]))),
          ],
          onChanged: (i) {
            if (i == null) return;
            app.setSetting(
              'auto_free_days',
              autoFreeCliValue(options[i]),
              success: 'Settings saved',
            );
          },
        ),
      ),
    );
  }

  Widget _keepFolders(BuildContext context, AppController app, AppConfig cfg) {
    final theme = FluentTheme.of(context);
    return Expander(
      initiallyExpanded: cfg.keepLocal.isNotEmpty,
      headerShape: (open) => RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(8),
        side: BorderSide(color: theme.cardStroke),
      ),
      leading: Icon(
        FluentIcons.pinned_solid,
        size: 18,
        color: theme.accentColor.defaultBrushFor(theme.brightness),
      ),
      header: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('Always keep on this computer'),
            const SizedBox(height: 2),
            Text(
              keepSummary(cfg.keepLocal.length),
              style: theme.typography.caption?.copyWith(color: theme.secondaryText),
            ),
          ],
        ),
      ),
      contentPadding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final rel in cfg.keepLocal)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 2),
              child: Row(
                children: [
                  const FileTypeIcon(name: '', isDir: true, size: 20),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(nameOf(rel)),
                        Text(
                          parentOf(rel).isEmpty ? 'CloudDrop' : parentOf(rel),
                          style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                        ),
                      ],
                    ),
                  ),
                  Tooltip(
                    message: 'Stop keeping this folder',
                    child: IconButton(
                      icon: const Icon(FluentIcons.remove, size: 12),
                      onPressed: () async {
                        await app.setKeep(rel, false);
                      },
                    ),
                  ),
                ],
              ),
            ),
          if (cfg.keepLocal.isEmpty)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: Text(
                'Folders you pin stay on this computer even when automatic space saving is on.',
                style: theme.typography.caption?.copyWith(color: theme.secondaryText),
              ),
            ),
          const SizedBox(height: 6),
          Align(
            alignment: Alignment.centerLeft,
            child: Button(
              onPressed: () => _addKeepFolder(app, cfg),
              child: const Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(FluentIcons.add, size: 12),
                  SizedBox(width: 8),
                  Text('Add Folder…'),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  Future<void> _addKeepFolder(AppController app, AppConfig cfg) async {
    final picked = await app.services.system.pickDirectory(
      title: 'Choose a Folder to Keep',
      initial: cfg.syncDir,
    );
    if (picked == null) return;
    final rel = app.paths.relInSync(cfg.syncDir, picked);
    if (rel == null) {
      app.say('Choose a folder inside ${app.paths.pretty(cfg.syncDir)}', Severity.warning);
      return;
    }
    if (addKeep(cfg.keepLocal, rel) == null) {
      app.say('“${nameOf(rel)}” is already kept on this computer');
      return;
    }
    await app.setKeep(rel, true);
  }

  // ---------- account ----------

  Widget _account(BuildContext context, AppController app) {
    final signedIn = app.snapshot.signedIn;
    final theme = FluentTheme.of(context);
    return _SettingCard(
      icon: FluentIcons.contact,
      title: 'Google Drive',
      subtitle: signedIn ? 'Signed in' : 'Not signed in',
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Button(
            onPressed: () => showSignInDialog(context, app),
            child: Text(signedIn ? 'Sign in again' : 'Sign in'),
          ),
          if (signedIn) ...[
            const SizedBox(width: 8),
            Button(
              onPressed: () => _signOut(context, app),
              style: ButtonStyle(
                foregroundColor: WidgetStatePropertyAll(theme.resources.systemFillColorCritical),
              ),
              child: const Text('Sign out'),
            ),
          ],
        ],
      ),
    );
  }

  Future<void> _signOut(BuildContext context, AppController app) async {
    if (await confirmSignOut(context)) await app.signOut();
  }

  // ---------- uploading ----------

  Widget _uploading(BuildContext context, AppController app, AppConfig cfg) {
    final theme = FluentTheme.of(context);
    return Column(
      children: [
        _SettingCard(
          icon: FluentIcons.cloud_upload,
          title: 'Files at a time',
          subtitle: 'More is faster on fast connections',
          trailing: SizedBox(
            width: 130,
            child: NumberBox<int>(
              value: cfg.parallelUploads,
              min: 1,
              max: 8,
              mode: SpinButtonPlacementMode.inline,
              onChanged: (v) {
                if (v == null || v == cfg.parallelUploads) return;
                _parallelTimer?.cancel();
                _parallelTimer = Timer(
                  const Duration(milliseconds: 400),
                  () => app.setSetting('parallel_uploads', '$v'),
                );
              },
            ),
          ),
        ),
        const SizedBox(height: 4),
        _SettingCard(
          icon: FluentIcons.timer,
          title: 'Wait after a change (seconds)',
          subtitle: 'Gives programs time to finish writing a file before it uploads',
          trailing: SizedBox(
            width: 130,
            child: NumberBox<int>(
              value: cfg.settleSecs,
              min: 0,
              max: 120,
              mode: SpinButtonPlacementMode.inline,
              onChanged: (v) {
                if (v == null || v == cfg.settleSecs) return;
                _settleTimer?.cancel();
                _settleTimer = Timer(
                  const Duration(milliseconds: 400),
                  () => app.setSetting('settle_secs', '$v'),
                );
              },
            ),
          ),
        ),
        const SizedBox(height: 4),
        _SettingCard(
          icon: FluentIcons.ringer,
          title: 'Notifications',
          subtitle: 'Tell me when uploads finish or something needs attention',
          trailing: ToggleSwitch(
            checked: cfg.notifications,
            onChanged: (v) => app.setSetting('notifications', '$v'),
          ),
        ),
        const SizedBox(height: 4),
        Surface(
          padding: const EdgeInsets.fromLTRB(16, 14, 16, 14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(FluentIcons.filter, size: 18, color: theme.secondaryText),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        const Text('Ignored files'),
                        const SizedBox(height: 2),
                        Text(
                          'Comma-separated patterns; matching files are never uploaded',
                          style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                        ),
                      ],
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              Row(
                children: [
                  Expanded(
                    child: TextBox(
                      controller: _ignore,
                      focusNode: _ignoreFocus,
                      placeholder: '*.tmp, *.part',
                      onSubmitted: (_) => _applyIgnore(app),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Button(onPressed: () => _applyIgnore(app), child: const Text('Apply')),
                ],
              ),
            ],
          ),
        ),
      ],
    );
  }

  Future<void> _applyIgnore(AppController app) async {
    final list = parseCommaList(_ignore.text);
    final r = await app.setSetting('ignore', commaList(list));
    if (r.ok) {
      _shownIgnore = commaList(list);
      _ignore.text = _shownIgnore;
    }
  }

  // ---------- background ----------

  Widget _background(BuildContext context, AppController app) {
    final svc = app.service;
    final running = app.snapshot.daemon != null;
    return Column(
      children: [
        _SettingCard(
          icon: FluentIcons.power_button,
          title: 'Start CloudDrop at sign-in',
          subtitle: 'Keeps uploading in the background after you sign in to Windows',
          trailing: ToggleSwitch(
            checked: svc?.startsAtSignIn ?? false,
            onChanged: svc == null ? null : app.setStartAtSignIn,
          ),
        ),
        const SizedBox(height: 4),
        _SettingCard(
          icon: FluentIcons.cloud_weather,
          title: 'Upload service',
          subtitle: running ? 'Running' : 'Not running',
          trailing: running
              ? null
              : Button(onPressed: app.startService, child: const Text('Start')),
        ),
      ],
    );
  }
}

/// One Windows 11 "settings card": icon, title and text on the left, the control on the right.
class _SettingCard extends StatelessWidget {
  const _SettingCard({
    required this.icon,
    required this.title,
    this.subtitle,
    this.trailing,
    this.stack = false,
  });

  /// On a narrow window, put a wide control (a drop-down) under the text.
  final bool stack;

  final IconData icon;
  final String title;
  final String? subtitle;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return LayoutBuilder(
      builder: (context, c) {
        final stacked = stack && c.maxWidth < 520 && trailing != null;
        final text = Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title),
            if (subtitle != null) ...[
              const SizedBox(height: 2),
              Text(
                subtitle!,
                style: theme.typography.caption?.copyWith(color: theme.secondaryText),
              ),
            ],
          ],
        );
        return Surface(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
          child: stacked
              ? Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Icon(icon, size: 18, color: theme.secondaryText),
                        const SizedBox(width: 14),
                        Expanded(child: text),
                      ],
                    ),
                    const SizedBox(height: 12),
                    Align(alignment: Alignment.centerLeft, child: trailing),
                  ],
                )
              : Row(
                  children: [
                    Icon(icon, size: 18, color: theme.secondaryText),
                    const SizedBox(width: 14),
                    Expanded(child: text),
                    if (trailing != null) ...[const SizedBox(width: 12), trailing!],
                  ],
                ),
        );
      },
    );
  }
}
