/// Home: one calm summary (with speed and time left while uploading), the uploads running
/// now, the everyday actions, two storage cards and the latest activity (`gui/src/home.rs`).
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../data/models.dart';
import '../../data/providers.dart';
import '../../logic/fmt.dart' as fmt;
import '../../logic/snapshot.dart';
import '../../state/activity_controller.dart';
import '../../state/app_controller.dart';
import '../scope.dart';
import '../theme.dart';
import '../widgets/common.dart';
import '../widgets/dialogs.dart';
import '../widgets/page_header.dart';
import '../widgets/sign_in_dialog.dart';

class HomePage extends StatelessWidget {
  const HomePage({super.key});

  @override
  Widget build(BuildContext context) {
    final scope = AppScope.of(context);
    final app = scope.app;
    return ScaffoldPage(
      padding: const EdgeInsets.only(top: 14),
      header: const AppPageHeader('Home'),
      content: ListenableBuilder(
        listenable: Listenable.merge([app, app.busyCount, scope.activity]),
        builder: (context, _) => _HomeBody(app: app, activity: scope.activity),
      ),
    );
  }
}

class _HomeBody extends StatelessWidget {
  const _HomeBody({required this.app, required this.activity});

  final AppController app;
  final ActivityController activity;

  @override
  Widget build(BuildContext context) {
    final s = app.snapshot;
    final sum = summarize(s);
    final uploads = s.daemon?.uploads ?? const <Upload>[];
    return PageBody(
      children: [
        _Hero(app: app, summary: sum),
        if (s.counts.failed > 0) ...[
          const SizedBox(height: 16),
          InfoBar(
            title: Text("${fmt.files(s.counts.failed)} couldn't be uploaded"),
            severity: InfoBarSeverity.error,
            isLong: false,
            action: Button(
              onPressed: () => app.showFiles(filter: Filter.failed),
              child: const Text('Review'),
            ),
          ),
        ],
        if (uploads.isNotEmpty) ...[
          const SectionTitle('Uploading now'),
          _UploadsCard(uploads: uploads),
        ],
        const SizedBox(height: 20),
        _Tiles(app: app),
        const SizedBox(height: 16),
        _Storage(app: app),
        if (activity.recent.isNotEmpty) ...[
          SectionTitle(
            'Recent activity',
            trailing: HyperlinkButton(
              onPressed: () => app.pane.value = AppPane.activity,
              child: const Text('See All'),
            ),
          ),
          Surface(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Column(
              children: [
                for (var i = 0; i < activity.recent.length; i++) ...[
                  if (i > 0)
                    const Padding(padding: EdgeInsets.symmetric(horizontal: 12), child: Divider()),
                  ActivityRow(
                    activity: activity.recent[i],
                    onOpen: (a) => app.showFiles(dir: parentOf(a.path), select: a.path),
                  ),
                ],
              ],
            ),
          ),
        ],
      ],
    );
  }
}

// ---------- hero ----------

class _Hero extends StatelessWidget {
  const _Hero({required this.app, required this.summary});

  final AppController app;
  final Summary summary;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final (icon, tint) = switch (summary.icon) {
      SummaryIcon.idle => (FluentIcons.completed_solid, theme.success),
      SummaryIcon.syncing => (FluentIcons.cloud_upload, theme.accent),
      SummaryIcon.paused => (FluentIcons.pause, theme.neutral),
      SummaryIcon.attention => (FluentIcons.warning, theme.caution),
    };
    final action = summary.action;
    final fraction = summary.fraction;
    final narrow = MediaQuery.widthOf(context) < 560;
    final align = narrow ? CrossAxisAlignment.center : CrossAxisAlignment.start;
    final textAlign = narrow ? TextAlign.center : TextAlign.start;
    final text = Column(
      crossAxisAlignment: align,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(summary.title, style: theme.typography.subtitle, textAlign: textAlign),
        const SizedBox(height: 4),
        Text(
          summary.detail,
          style: theme.typography.body?.copyWith(color: theme.secondaryText),
          textAlign: textAlign,
        ),
        if (fraction != null) ...[
          const SizedBox(height: 12),
          WideProgressBar(fraction: fraction),
          const SizedBox(height: 6),
          Text(
            summary.speed ?? 'Measuring speed…',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
            textAlign: textAlign,
          ),
        ],
        if (action != null) ...[
          const SizedBox(height: 14),
          FilledButton(
            onPressed: () => _run(context, action),
            child: Text(_label(action, app.provider)),
          ),
        ],
      ],
    );
    final badge = Container(
      width: 72,
      height: 72,
      decoration: BoxDecoration(
        color: theme.isDark ? const Color(0x1FFFFFFF) : const Color(0xE6FFFFFF),
        shape: BoxShape.circle,
        boxShadow: theme.isDark
            ? null
            : const [BoxShadow(color: Color(0x1A1F3A8A), blurRadius: 12, offset: Offset(0, 3))],
      ),
      alignment: Alignment.center,
      child: Icon(icon, size: 34, color: tint.fg),
    );
    return DecoratedBox(
      decoration: BoxDecoration(
        gradient: Brand.wash(theme.brightness),
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: theme.cardStroke),
      ),
      child: Padding(
        padding: EdgeInsets.all(narrow ? 20 : 24),
        child: narrow
            ? Column(children: [badge, const SizedBox(height: 14), text])
            : Row(
                children: [
                  badge,
                  const SizedBox(width: 20),
                  Expanded(child: text),
                ],
              ),
      ),
    );
  }

  String _label(SummaryAction a, CloudProvider p) => switch (a) {
    SummaryAction.setup => 'Set Up CloudDrop',
    SummaryAction.signIn => p.signIn == SignInKind.form ? 'Connect' : 'Sign In',
    SummaryAction.startService => 'Start CloudDrop',
    SummaryAction.resume => 'Resume Uploads',
  };

  void _run(BuildContext context, SummaryAction a) {
    switch (a) {
      case SummaryAction.setup:
        app.beginSetup();
      case SummaryAction.signIn:
        showSignInDialog(context, app);
      case SummaryAction.startService:
        app.startService();
      case SummaryAction.resume:
        app.setPaused(false);
    }
  }
}

// ---------- uploads ----------

class _UploadsCard extends StatelessWidget {
  const _UploadsCard({required this.uploads});

  final List<Upload> uploads;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return Surface(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
      child: Column(
        children: [
          for (var i = 0; i < uploads.length; i++) ...[
            if (i > 0) const Divider(),
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Row(
                children: [
                  FileTypeIcon(name: nameOf(uploads[i].path), isDir: false),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(nameOf(uploads[i].path), maxLines: 1, overflow: TextOverflow.ellipsis),
                        Text(
                          parentOf(uploads[i].path).isEmpty
                              ? AppScope.of(context).app.rootName
                              : parentOf(uploads[i].path),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 12),
                  SizedBox(
                    width: 150,
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.end,
                      children: [
                        WideProgressBar(fraction: uploads[i].fraction),
                        const SizedBox(height: 4),
                        Text(
                          '${fmt.humanBytes(uploads[i].sent)} / ${fmt.humanBytes(uploads[i].total)}',
                          style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                        ),
                      ],
                    ),
                  ),
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

// ---------- quick actions ----------

class _Tiles extends StatelessWidget {
  const _Tiles({required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final s = app.snapshot;
    final busy = app.busy;
    final paused = s.paused;
    final tiles = [
      _Tile(
        icon: FluentIcons.open_folder_horizontal,
        label: 'Open Folder',
        tooltip: app.several
            ? 'Open “${app.label}” in File Explorer'
            : 'Open the CloudDrop folder in File Explorer',
        onPressed: app.syncDir == null ? null : app.openFolder,
      ),
      _Tile(
        icon: FluentIcons.add,
        label: 'Add Files…',
        tooltip: 'Copy files into ${app.rootName} to upload them',
        onPressed: s.ready ? () => app.addFiles('') : null,
      ),
      _Tile(
        icon: FluentIcons.broom,
        label: 'Free Up Space…',
        tooltip: 'Remove local copies of uploaded files; they stay in ${app.provider.name}',
        onPressed: s.counts.synced > 0 && !busy ? () => _free(context) : null,
      ),
      _Tile(
        icon: paused ? FluentIcons.play : FluentIcons.pause,
        label: paused ? 'Resume Uploads' : 'Pause Uploads',
        tooltip: paused ? 'Continue uploading' : 'Stop uploading until you resume',
        onPressed: s.ready ? () => app.setPaused(!paused) : null,
      ),
    ];
    return LayoutBuilder(
      builder: (context, c) {
        final cols = c.maxWidth >= 600 ? 4 : 2;
        const gap = 12.0;
        final w = (c.maxWidth - gap * (cols - 1)) / cols;
        return Wrap(
          spacing: gap,
          runSpacing: gap,
          children: [for (final t in tiles) SizedBox(width: w, child: t)],
        );
      },
    );
  }

  Future<void> _free(BuildContext context) async {
    final synced = app.snapshot.counts.synced;
    final ok = await confirmFree(
      context,
      body:
          'This removes the local copies of all $synced uploaded files. They stay in '
          '${app.provider.name}, and you can download them again from Files at any time. '
          'Folders are kept.',
    );
    if (ok) await app.free(['']);
  }
}

class _Tile extends StatelessWidget {
  const _Tile({required this.icon, required this.label, required this.tooltip, this.onPressed});

  final IconData icon;
  final String label;
  final String tooltip;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final enabled = onPressed != null;
    return Tooltip(
      message: tooltip,
      child: HoverButton(
        onPressed: onPressed,
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        builder: (context, states) {
          final hovered = states.isHovered || states.isFocused;
          final fill = hovered ? theme.resources.cardBackgroundFillColorSecondary : theme.cardFill;
          return Container(
            height: 92,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            decoration: BoxDecoration(
              color: fill,
              borderRadius: BorderRadius.circular(8),
              border: Border.all(
                color: hovered ? theme.resources.controlStrokeColorSecondary : theme.cardStroke,
              ),
            ),
            child: Opacity(
              opacity: enabled ? 1 : 0.45,
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Icon(icon, size: 24, color: theme.accentColor.defaultBrushFor(theme.brightness)),
                  const SizedBox(height: 10),
                  Text(
                    label,
                    maxLines: 2,
                    textAlign: TextAlign.center,
                    overflow: TextOverflow.ellipsis,
                  ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}

// ---------- storage ----------

class _Storage extends StatelessWidget {
  const _Storage({required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final s = app.snapshot;
    final drive = _driveCard(context, s);
    final computer = _computerCard(context, s);
    return LayoutBuilder(
      builder: (context, c) {
        if (c.maxWidth >= 600) {
          return IntrinsicHeight(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Expanded(child: drive),
                const SizedBox(width: 12),
                Expanded(child: computer),
              ],
            ),
          );
        }
        return Column(children: [drive, const SizedBox(height: 12), computer]);
      },
    );
  }

  Widget _driveCard(BuildContext context, Snapshot s) {
    final text = cloudStorage(s);
    final p = s.provider;
    return _StorageCard(
      icon: providerIcon(p.id),
      title: p.name,
      headline: text.headline,
      muted: text.muted,
      fraction: text.fraction,
      note: text.note,
      // Providers without web pages (WebDAV, S3, SFTP) have nothing to open.
      actionIcon: p.webLinks ? FluentIcons.open_in_new_window : null,
      actionTooltip: 'Open in ${p.name}',
      onAction: s.ready ? app.openRootInDrive : null,
      warnWhenFull: true,
    );
  }

  Widget _computerCard(BuildContext context, Snapshot s) {
    final total = s.root.bytes < 0 ? 0 : s.root.bytes;
    final freed = s.root.cloudOnlyBytes < 0 ? 0 : s.root.cloudOnlyBytes;
    final stored = total > freed ? total - freed : 0;
    if (s.root.files == 0) {
      return _StorageCard(
        icon: FluentIcons.pc1,
        title: 'This computer',
        headline: 'Nothing yet',
        muted: true,
        note: 'Files you add to ${app.rootName} show up here',
        actionIcon: FluentIcons.open_folder_horizontal,
        actionTooltip: 'Open folder',
        onAction: app.syncDir == null ? null : app.openFolder,
      );
    }
    return _StorageCard(
      icon: FluentIcons.pc1,
      title: 'This computer',
      headline: freed == 0 ? 'Nothing freed yet' : '${fmt.humanBytes(freed)} freed',
      fraction: total == 0 ? 0 : freed / total,
      note:
          '${fmt.humanBytes(stored)} still stored here · ${fmt.files(s.root.local)} on this computer',
      actionIcon: FluentIcons.open_folder_horizontal,
      actionTooltip: 'Open folder',
      onAction: app.syncDir == null ? null : app.openFolder,
    );
  }
}

class _StorageCard extends StatelessWidget {
  const _StorageCard({
    required this.icon,
    required this.title,
    required this.headline,
    required this.note,
    required this.actionIcon,
    required this.actionTooltip,
    this.onAction,
    this.fraction,
    this.muted = false,
    this.warnWhenFull = false,
  });

  final IconData icon;
  final String title;
  final String headline;
  final String note;
  final double? fraction;
  final bool muted;

  /// A nearly full bar turns amber and red (storage quota); a "freed" bar never does.
  final bool warnWhenFull;

  /// null: no action (the row keeps its height).
  final IconData? actionIcon;
  final String actionTooltip;
  final VoidCallback? onAction;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final f = fraction?.clamp(0.0, 1.0);
    final barColor = f == null || !warnWhenFull
        ? null
        : f >= 0.95
        ? theme.resources.systemFillColorCritical
        : f >= 0.85
        ? theme.resources.systemFillColorCaution
        : null;
    return Surface(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(icon, size: 16, color: theme.secondaryText),
              const SizedBox(width: 8),
              Expanded(child: Text(title, style: theme.typography.bodyStrong)),
              if (actionIcon != null)
                Tooltip(
                  message: actionTooltip,
                  child: IconButton(icon: Icon(actionIcon, size: 14), onPressed: onAction),
                )
              else
                // Same height as the button, so both cards line up.
                Visibility(
                  visible: false,
                  maintainSize: true,
                  maintainAnimation: true,
                  maintainState: true,
                  child: IconButton(icon: const Icon(FluentIcons.cloud, size: 14), onPressed: null),
                ),
            ],
          ),
          const SizedBox(height: 8),
          Text(
            headline,
            style: theme.typography.subtitle?.copyWith(color: muted ? theme.secondaryText : null),
          ),
          if (f != null) ...[
            const SizedBox(height: 10),
            WideProgressBar(fraction: f, color: barColor),
          ],
          const SizedBox(height: 10),
          Text(note, style: theme.typography.caption?.copyWith(color: theme.secondaryText)),
        ],
      ),
    );
  }
}
