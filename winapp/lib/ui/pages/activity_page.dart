/// Activity: what CloudDrop did, newest first, grouped by day, with a filter by kind
/// (`gui/src/activity.rs`).
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
      header: AppPageHeader(
        'Activity',
        trailing: ListenableBuilder(
          listenable: c,
          builder: (context, _) => c.items.isEmpty
              ? const SizedBox.shrink()
              : SizedBox(
                  width: 170,
                  child: ComboBox<ActivityFilter>(
                    isExpanded: true,
                    value: c.filter,
                    items: [
                      for (final f in ActivityFilter.values)
                        ComboBoxItem<ActivityFilter>(
                          value: f,
                          child: Text(f.label, overflow: TextOverflow.ellipsis),
                        ),
                    ],
                    onChanged: (f) {
                      if (f != null) c.filter = f;
                    },
                  ),
                ),
        ),
      ),
      content: ListenableBuilder(
        listenable: c,
        builder: (context, _) {
          if (!c.loaded) return const Center(child: ProgressRing());
          if (c.items.isEmpty) return const _Empty(filter: ActivityFilter.all);
          if (c.visibleItems.isEmpty) return _Empty(filter: c.filter);
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
  const _Empty({required this.filter});

  final ActivityFilter filter;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(
            filter == ActivityFilter.all ? FluentIcons.history : FluentIcons.filter,
            size: 44,
            color: theme.secondaryText,
          ),
          const SizedBox(height: 14),
          Text(
            filter == ActivityFilter.all ? 'No Activity Yet' : 'Nothing to Show',
            style: theme.typography.subtitle,
          ),
          const SizedBox(height: 6),
          Text(
            filter.emptyText,
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
                  ActivityRow(activity: g.items[i], onOpen: onOpen),
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
