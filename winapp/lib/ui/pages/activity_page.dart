/// Activity: what CloudDrop did, newest first, grouped by day (`gui/src/activity.rs`).
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../data/models.dart';
import '../../logic/activity_text.dart';
import '../../logic/fmt.dart' as fmt;
import '../../state/activity_controller.dart';
import '../scope.dart';
import '../theme.dart';
import '../widgets/common.dart';
import '../widgets/page_header.dart';

class ActivityPage extends StatelessWidget {
  const ActivityPage({super.key});

  @override
  Widget build(BuildContext context) {
    final scope = AppScope.of(context);
    final c = scope.activity;
    return ScaffoldPage(
      padding: const EdgeInsets.only(top: 14),
      header: const AppPageHeader('Activity'),
      content: ListenableBuilder(
        listenable: c,
        builder: (context, _) {
          if (!c.loaded) return const Center(child: ProgressRing());
          if (c.items.isEmpty) return const _Empty();
          return _List(
            controller: c,
            onOpen: (a) => scope.app.showFiles(dir: parentOf(a.path), select: a.path),
          );
        },
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(FluentIcons.history, size: 44, color: theme.secondaryText),
          const SizedBox(height: 14),
          Text('No Activity Yet', style: theme.typography.subtitle),
          const SizedBox(height: 6),
          Text(
            'Uploads, downloads and other changes will be listed here.',
            style: theme.typography.body?.copyWith(color: theme.secondaryText),
            textAlign: TextAlign.center,
          ),
        ],
      ),
    );
  }
}

class _List extends StatelessWidget {
  const _List({required this.controller, required this.onOpen});

  final ActivityController controller;
  final void Function(Activity a) onOpen;

  @override
  Widget build(BuildContext context) {
    final today = fmt.nowCivil();
    final groups = fmt.groupByDay<Activity>(
      controller.visibleItems,
      (a) => a.atMs,
      fmt.civilFromMs,
      today,
    );
    return PageBody(
      children: [
        for (final g in groups) ...[
          SectionTitle(g.heading),
          Surface(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Column(
              children: [
                for (var i = 0; i < g.items.length; i++) ...[
                  if (i > 0)
                    const Padding(padding: EdgeInsets.symmetric(horizontal: 12), child: Divider()),
                  _Row(activity: g.items[i], onOpen: onOpen),
                ],
              ],
            ),
          ),
        ],
        if (controller.hasMore)
          Padding(
            padding: const EdgeInsets.only(top: 20),
            child: Center(
              child: Button(
                onPressed: controller.showMore,
                child: const Text('Show Older Activity'),
              ),
            ),
          ),
      ],
    );
  }
}

class _Row extends StatelessWidget {
  const _Row({required this.activity, required this.onOpen});

  final Activity activity;
  final void Function(Activity a) onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final d = describe(activity);
    final sub = subtitle(activity, fmt.civilFromMs(activity.atMs));
    return Tooltip(
      message: activity.path,
      child: HoverButton(
        onPressed: () => onOpen(activity),
        builder: (context, states) => Container(
          color: states.isHovered ? theme.resources.subtleFillColorSecondary : Colors.transparent,
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
          child: Row(
            children: [
              IconBadge(icon: activityIcon(d.icon), tint: tintFor(theme, d.tone), size: 34),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(d.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                    const SizedBox(height: 2),
                    Text(
                      sub,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              Icon(FluentIcons.chevron_right, size: 11, color: theme.secondaryText),
            ],
          ),
        ),
      ),
    );
  }
}
