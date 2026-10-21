/// The connection switcher: with several folders uploading, each one's label and status dot,
/// in the navigation pane above Home (a drop-down in the top bar of a narrow window), and
/// "Add Another Folder…".
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../logic/snapshot.dart';
import '../../state/app_controller.dart';
import '../theme.dart';

/// The colour of a status dot: green up to date, accent uploading, grey paused, amber when
/// something needs attention (the status chip's colours).
Color statusDotColor(FluentThemeData theme, SummaryIcon icon) => switch (icon) {
  SummaryIcon.idle => theme.success.fg,
  SummaryIcon.syncing => theme.accent.fg,
  SummaryIcon.paused => theme.secondaryText,
  SummaryIcon.attention => theme.caution.fg,
};

/// A status dot.
class StatusDot extends StatelessWidget {
  const StatusDot(this.icon, {super.key, this.size = 8});

  final SummaryIcon icon;
  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(
      color: statusDotColor(FluentTheme.of(context), icon),
      shape: BoxShape.circle,
    ),
  );
}

/// A folder with its status dot: the switcher's icon for a connection.
class ConnectionIcon extends StatelessWidget {
  const ConnectionIcon(this.icon, {super.key});

  final SummaryIcon icon;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return SizedBox(
      width: 18,
      height: 16,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          const Positioned(
            left: 0,
            top: 0,
            child: Icon(FluentIcons.fabric_folder_fill, size: 16, color: Color(0xFFF2B63C)),
          ),
          Positioned(
            right: -1,
            bottom: -2,
            child: Container(
              width: 10,
              height: 10,
              decoration: BoxDecoration(
                color: statusDotColor(theme, icon),
                shape: BoxShape.circle,
                // A ring of the pane's colour sets the dot off the folder.
                border: Border.all(color: theme.resources.solidBackgroundFillColorBase, width: 1.5),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// What a connection's row says when hovered: its label and status line.
String connectionTooltip(ConnectionEntry c) => '${c.label} · ${c.line}';

/// The pane items above Home (expanded and compact pane): a "Folders" heading, one item per
/// connection, "Add Another Folder…" and a separator. Empty with one connection.
List<NavigationPaneItem> connectionPaneItems(
  BuildContext context,
  AppController app,
  PaneDisplayMode mode,
) {
  final rows = app.switcher;
  if (rows.isEmpty) return const [];
  final theme = FluentTheme.of(context);
  return [
    if (mode == PaneDisplayMode.expanded) PaneItemHeader(header: const Text('Folders')),
    for (final c in rows)
      PaneItemAction(
        key: ValueKey('connection:${c.name}'),
        icon: Semantics(
          label: connectionTooltip(c),
          selected: c.selected,
          child: ConnectionIcon(c.icon),
        ),
        title: Text(
          c.label,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: c.selected ? const TextStyle(fontWeight: FontWeight.w600) : null,
        ),
        trailing: c.selected
            ? Padding(
                padding: const EdgeInsetsDirectional.only(end: 10),
                child: Icon(
                  FluentIcons.check_mark,
                  size: 12,
                  color: theme.accentColor.defaultBrushFor(theme.brightness),
                ),
              )
            : null,
        onTap: () => app.selectConnection(c.name),
      ),
    PaneItemAction(
      key: const ValueKey('connection:add'),
      icon: const Icon(FluentIcons.add, size: 14),
      title: const Text('Add Another Folder…'),
      onTap: app.beginAddConnection,
    ),
    PaneItemSeparator(),
  ];
}

/// The switcher of a narrow window (tabs along the top): the selected connection, opening a
/// list of all of them and "Add Another Folder…".
class ConnectionMenu extends StatelessWidget {
  const ConnectionMenu({super.key, required this.app});

  final AppController app;

  @override
  Widget build(BuildContext context) {
    final rows = app.switcher;
    final theme = FluentTheme.of(context);
    final current = rows.where((c) => c.selected).firstOrNull ?? rows.first;
    return Tooltip(
      message: connectionTooltip(current),
      child: DropDownButton(
        title: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            ConnectionIcon(current.icon),
            const SizedBox(width: 8),
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 96),
              child: Text(current.label, maxLines: 1, overflow: TextOverflow.ellipsis),
            ),
          ],
        ),
        items: [
          for (final c in rows)
            MenuFlyoutItem(
              leading: ConnectionIcon(c.icon),
              text: Text(c.label),
              trailing: c.selected
                  ? Icon(
                      FluentIcons.check_mark,
                      size: 12,
                      color: theme.accentColor.defaultBrushFor(theme.brightness),
                    )
                  : null,
              onPressed: () => app.selectConnection(c.name),
            ),
          const MenuFlyoutSeparator(),
          MenuFlyoutItem(
            leading: const Icon(FluentIcons.add, size: 14),
            text: const Text('Add Another Folder…'),
            onPressed: app.beginAddConnection,
          ),
        ],
      ),
    );
  }
}
