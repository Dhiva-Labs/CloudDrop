/// Files: a browser for everything in CloudDrop, on this computer or only in Drive
/// (`gui/src/files.rs`).
library;

import 'dart:async';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/services.dart';

import '../../data/models.dart';
import '../../logic/files_model.dart';
import '../../logic/fmt.dart' as fmt;
import '../../state/files_controller.dart';
import '../scope.dart';
import '../theme.dart';
import '../widgets/common.dart';
import '../widgets/dialogs.dart';

const double _rowHeight = 48;
const double _narrowWidth = 640;

class FilesPage extends StatefulWidget {
  const FilesPage({super.key, this.enableDrop = true});

  /// Off in tests: the native drop plugin isn't there.
  final bool enableDrop;

  @override
  State<FilesPage> createState() => _FilesPageState();
}

class _FilesPageState extends State<FilesPage> {
  final _scroll = ScrollController();
  final _search = TextEditingController();
  final _searchFocus = FocusNode();
  final _listFocus = FocusNode(debugLabel: 'files list');
  final _menu = FlyoutController();
  final _menuKey = GlobalKey();
  FilesController? _files;
  int _resetSeen = 0;
  bool _dragging = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final files = AppScope.of(context).files;
    if (!identical(files, _files)) {
      _files?.removeListener(_onChange);
      _files = files..addListener(_onChange);
      _resetSeen = files.resetCount;
    }
  }

  @override
  void dispose() {
    _files?.removeListener(_onChange);
    _scroll.dispose();
    _search.dispose();
    _searchFocus.dispose();
    _listFocus.dispose();
    _menu.dispose();
    super.dispose();
  }

  void _onChange() {
    final files = _files!;
    // The search box follows the controller when the page itself cleared the search.
    if (_search.text != files.searchText) {
      _search.value = TextEditingValue(
        text: files.searchText,
        selection: TextSelection.collapsed(offset: files.searchText.length),
      );
    }
    final reset = files.resetCount != _resetSeen;
    _resetSeen = files.resetCount;
    final target = files.scrollTarget;
    files.scrollTarget = null;
    if (reset || target != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!mounted || !_scroll.hasClients) return;
        if (target != null) {
          final i = files.items.indexWhere((e) => e.path == target);
          if (i >= 0) {
            final view = _scroll.position.viewportDimension;
            final want = i * _rowHeight - (view - _rowHeight) / 2;
            _scroll.jumpTo(want.clamp(0, _scroll.position.maxScrollExtent));
            return;
          }
        }
        if (reset) _scroll.jumpTo(0);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final scope = AppScope.of(context);
    final files = scope.files;
    final app = scope.app;
    return ScaffoldPage(
      padding: EdgeInsets.zero,
      content: ListenableBuilder(
        listenable: Listenable.merge([files, app.busyCount, app]),
        builder: (context, _) {
          return LayoutBuilder(
            builder: (context, c) {
              final narrow = c.maxWidth < _narrowWidth;
              final gutter = pageGutter(c.maxWidth);
              return Focus(
                onKeyEvent: (node, event) => _onKey(files, event),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    _Toolbar(
                      files: files,
                      narrow: narrow,
                      gutter: gutter,
                      search: _search,
                      searchFocus: _searchFocus,
                    ),
                    _SummaryRow(files: files, gutter: gutter),
                    AnimatedSize(
                      duration: const Duration(milliseconds: 150),
                      alignment: Alignment.topCenter,
                      child: files.selectedPaths.isEmpty
                          ? const SizedBox(width: double.infinity)
                          : _SelectionBar(
                              files: files,
                              busy: app.busy,
                              gutter: gutter,
                              narrow: narrow,
                              width: c.maxWidth,
                            ),
                    ),
                    Expanded(child: _content(context, files, narrow, gutter)),
                  ],
                ),
              );
            },
          );
        },
      ),
    );
  }

  KeyEventResult _onKey(FilesController files, KeyEvent event) {
    if (event is! KeyDownEvent) return KeyEventResult.ignored;
    final key = event.logicalKey;
    final kb = HardwareKeyboard.instance;
    final ctrl = kb.isControlPressed || kb.isMetaPressed;
    // Typing in the search box keeps its own keys.
    if (_searchFocus.hasFocus && key != LogicalKeyboardKey.escape) {
      return KeyEventResult.ignored;
    }
    if (key == LogicalKeyboardKey.escape) {
      files.clearSelection();
      return KeyEventResult.handled;
    }
    if (key == LogicalKeyboardKey.backspace ||
        (kb.isAltPressed && key == LogicalKeyboardKey.arrowLeft)) {
      files.goBack();
      return KeyEventResult.handled;
    }
    if (ctrl && key == LogicalKeyboardKey.keyA) {
      files.selectAll();
      return KeyEventResult.handled;
    }
    if (ctrl && key == LogicalKeyboardKey.keyF) {
      _searchFocus.requestFocus();
      return KeyEventResult.handled;
    }
    if (key == LogicalKeyboardKey.enter) {
      final sel = files.selectedItems;
      if (sel.length == 1) {
        files.activate(sel.first);
        return KeyEventResult.handled;
      }
    }
    return KeyEventResult.ignored;
  }

  Widget _content(BuildContext context, FilesController files, bool narrow, double gutter) {
    final theme = FluentTheme.of(context);
    Widget body;
    if (files.error != null) {
      body = Padding(
        padding: EdgeInsets.all(gutter),
        child: InfoBar(title: Text(files.error!), severity: InfoBarSeverity.error),
      );
    } else if (!files.loaded) {
      body = const Center(child: ProgressRing());
    } else if (files.items.isEmpty) {
      body = _EmptyState(files: files);
    } else {
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _HeaderRow(files: files, narrow: narrow, gutter: gutter),
          Expanded(
            child: FlyoutTarget(
              key: _menuKey,
              controller: _menu,
              child: ListView.builder(
                controller: _scroll,
                itemExtent: _rowHeight,
                itemCount: files.items.length,
                padding: EdgeInsets.symmetric(horizontal: gutter - 8 < 4 ? 4 : gutter - 8),
                itemBuilder: (context, i) {
                  final it = files.items[i];
                  return _FileRow(
                    key: ValueKey(it.path),
                    files: files,
                    item: it,
                    narrow: narrow,
                    onMenu: (pos) => _showMenu(context, files, it, pos),
                    onFocusList: _listFocus.requestFocus,
                  );
                },
              ),
            ),
          ),
        ],
      );
    }
    final dropArea = Stack(
      children: [
        Positioned.fill(child: body),
        if (_dragging)
          Positioned.fill(
            child: IgnorePointer(
              child: Container(
                margin: const EdgeInsets.all(8),
                decoration: BoxDecoration(
                  color: theme.accent.bg,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(
                    color: theme.accentColor.defaultBrushFor(theme.brightness),
                    width: 2,
                  ),
                ),
                alignment: Alignment.center,
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    IconBadge(icon: FluentIcons.cloud_upload, tint: theme.accent, size: 64),
                    const SizedBox(height: 12),
                    Text('Drop to add to CloudDrop', style: theme.typography.subtitle),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
    if (!widget.enableDrop) return Focus(focusNode: _listFocus, child: dropArea);
    return DropTarget(
      onDragEntered: (_) => setState(() => _dragging = true),
      onDragExited: (_) => setState(() => _dragging = false),
      onDragDone: (d) {
        setState(() => _dragging = false);
        files.dropped([for (final f in d.files) f.path]);
      },
      child: Focus(focusNode: _listFocus, child: dropArea),
    );
  }

  // ---------- context menu ----------

  void _showMenu(BuildContext context, FilesController files, DirItem it, Offset position) {
    if (!files.isSelected(it.path)) files.selectOnly(it.path);
    final caps = files.caps;
    final busy = AppScope.of(context).app.busy;
    final sel = files.selectedItems;
    final items = <MenuFlyoutItemBase>[
      if (caps.count == 1)
        MenuFlyoutItem(
          leading: const Icon(FluentIcons.open_file),
          text: const Text('Open'),
          onPressed: () => files.activate(sel.first),
        ),
      if (caps.download)
        MenuFlyoutItem(
          leading: const Icon(FluentIcons.cloud_download),
          text: const Text('Download'),
          onPressed: busy ? null : files.downloadSelected,
        ),
      if (caps.free)
        MenuFlyoutItem(
          leading: const Icon(FluentIcons.hard_drive),
          text: const Text('Free Up Space'),
          onPressed: busy ? null : () => _free(context, files),
        ),
      if (caps.retry)
        MenuFlyoutItem(
          leading: const Icon(FluentIcons.refresh),
          text: const Text('Retry Upload'),
          onPressed: files.retrySelected,
        ),
      const MenuFlyoutSeparator(),
      MenuFlyoutItem(
        leading: const Icon(FluentIcons.open_in_new_window),
        text: const Text('Open in Google Drive'),
        onPressed: caps.openDrive ? files.openSelectedInDrive : null,
      ),
      MenuFlyoutItem(
        leading: const Icon(FluentIcons.fabric_folder),
        text: const Text('Show in File Explorer'),
        onPressed: files.showSelectedInExplorer,
      ),
      if (caps.keep == Keep.on || caps.keep == Keep.off) ...[
        const MenuFlyoutSeparator(),
        MenuFlyoutItem(
          leading: Icon(caps.keep == Keep.on ? FluentIcons.unpin : FluentIcons.pin),
          text: Text(
            caps.keep == Keep.on ? 'Stop Keeping on This Computer' : 'Always Keep on This Computer',
          ),
          onPressed: () => files.setKeepSelected(caps.keep == Keep.off),
        ),
      ],
    ];
    _menu.showFlyout<void>(
      barrierColor: Colors.transparent,
      position: position,
      placementMode: FlyoutPlacementMode.bottomLeft,
      builder: (context) => MenuFlyout(items: items),
    );
  }
}

Future<void> _free(BuildContext context, FilesController files) async {
  final sel = files.selectedItems;
  if (sel.isEmpty) return;
  final body = sel.length == 1 && !sel.first.isDir
      ? 'This removes the local copy of ${files.what(sel)}. It stays in Google Drive, and '
            'you can download it again at any time.'
      : 'This removes the local copies of the uploaded files in ${files.what(sel)}. They '
            'stay in Google Drive, and you can download them again at any time. Folders are kept.';
  final ok = await confirmFree(context, body: body);
  if (ok) await files.freeSelected();
}

// ---------- toolbar ----------

class _Toolbar extends StatelessWidget {
  const _Toolbar({
    required this.files,
    required this.narrow,
    required this.gutter,
    required this.search,
    required this.searchFocus,
  });

  final FilesController files;
  final bool narrow;
  final double gutter;
  final TextEditingController search;
  final FocusNode searchFocus;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return Padding(
      padding: EdgeInsets.fromLTRB(gutter, 12, gutter, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Tooltip(
                message: files.search.isNotEmpty
                    ? 'Back to the folder'
                    : files.dir.isNotEmpty
                    ? 'Back to the parent folder (Alt+←)'
                    : 'Back to all files',
                child: IconButton(
                  icon: const Icon(FluentIcons.back, size: 14),
                  onPressed: files.canGoBack ? files.goBack : null,
                ),
              ),
              const SizedBox(width: 4),
              Expanded(child: _Crumbs(files: files)),
              const SizedBox(width: 8),
              Tooltip(
                message: 'Copy files into this folder',
                child: FilledButton(
                  onPressed: files.addFiles,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Icon(FluentIcons.add, size: 12),
                      if (!narrow) ...[const SizedBox(width: 8), const Text('Add Files')],
                    ],
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 10),
          Row(
            children: [
              Expanded(
                child: TextBox(
                  controller: search,
                  focusNode: searchFocus,
                  placeholder: 'Search all files',
                  onChanged: files.searchChanged,
                  prefix: Padding(
                    padding: const EdgeInsetsDirectional.only(start: 10),
                    child: Icon(FluentIcons.search, size: 13, color: theme.secondaryText),
                  ),
                  suffix: files.searchText.isEmpty
                      ? null
                      : IconButton(
                          icon: const Icon(FluentIcons.clear, size: 10),
                          onPressed: () {
                            search.clear();
                            files.searchChanged('');
                          },
                        ),
                ),
              ),
              const SizedBox(width: 8),
              SizedBox(
                width: narrow ? 150 : 190,
                child: ComboBox<Filter>(
                  isExpanded: true,
                  value: files.filter,
                  items: [
                    for (final f in Filter.values)
                      ComboBoxItem<Filter>(
                        value: f,
                        child: Text(f.label, overflow: TextOverflow.ellipsis),
                      ),
                  ],
                  onChanged: (f) {
                    if (f != null) files.setFilter(f);
                  },
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Crumbs extends StatelessWidget {
  const _Crumbs({required this.files});

  final FilesController files;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final segments = <(String label, String path)>[('CloudDrop', '')];
    if (files.search.isEmpty) {
      var acc = '';
      for (final part in files.dir.split('/').where((p) => p.isNotEmpty)) {
        acc = joinRel(acc, part);
        segments.add((part, acc));
      }
    }
    final lastIsCurrent = files.search.isEmpty;
    final children = <Widget>[];
    for (var i = 0; i < segments.length; i++) {
      final (label, path) = segments[i];
      if (i > 0) {
        children.add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 2),
            child: Icon(FluentIcons.chevron_right, size: 10, color: theme.secondaryText),
          ),
        );
      }
      if (lastIsCurrent && i == segments.length - 1) {
        children.add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: Text(label, style: theme.typography.bodyStrong),
          ),
        );
      } else {
        children.add(
          HyperlinkButton(
            onPressed: () => files.show(path, filter: files.filter),
            style: ButtonStyle(
              padding: WidgetStatePropertyAll(
                const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
              ),
              foregroundColor: WidgetStatePropertyAll(theme.resources.textFillColorPrimary),
            ),
            child: Text(label),
          ),
        );
      }
    }
    if (files.search.isNotEmpty) {
      children
        ..add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 2),
            child: Icon(FluentIcons.chevron_right, size: 10, color: theme.secondaryText),
          ),
        )
        ..add(
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 260),
              child: Text(
                'Search “${files.search}”',
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.typography.bodyStrong,
              ),
            ),
          ),
        );
    }
    return _CrumbScroller(children: children);
  }
}

/// The breadcrumb row: starts at the left, and scrolls to the end when it is too long.
class _CrumbScroller extends StatefulWidget {
  const _CrumbScroller({required this.children});

  final List<Widget> children;

  @override
  State<_CrumbScroller> createState() => _CrumbScrollerState();
}

class _CrumbScrollerState extends State<_CrumbScroller> {
  final _controller = ScrollController();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_controller.hasClients) {
        _controller.jumpTo(_controller.position.maxScrollExtent);
      }
    });
    return SizedBox(
      height: 32,
      child: SingleChildScrollView(
        controller: _controller,
        scrollDirection: Axis.horizontal,
        child: Row(mainAxisSize: MainAxisSize.min, children: widget.children),
      ),
    );
  }
}

class _SummaryRow extends StatelessWidget {
  const _SummaryRow({required this.files, required this.gutter});

  final FilesController files;
  final double gutter;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final text = files.summary;
    final showRetry = files.filter == Filter.failed && files.items.isNotEmpty;
    if (text.isEmpty && !showRetry) return const SizedBox(height: 4);
    return Padding(
      padding: EdgeInsets.fromLTRB(gutter + 4, 0, gutter, 6),
      child: Row(
        children: [
          Expanded(
            child: Text(
              text,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.typography.caption?.copyWith(color: theme.secondaryText),
            ),
          ),
          if (showRetry) Button(onPressed: files.retryAllHere, child: const Text('Retry All')),
        ],
      ),
    );
  }
}

// ---------- selection bar ----------

class _SelectionBar extends StatelessWidget {
  const _SelectionBar({
    required this.files,
    required this.busy,
    required this.gutter,
    required this.narrow,
    required this.width,
  });

  final FilesController files;
  final bool busy;
  final double gutter;
  final bool narrow;
  final double width;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final caps = files.caps;
    final sel = files.selectedItems;
    final label = sel.length == 1 ? sel.first.name : '${caps.count} selected';

    // Wide enough: the main commands carry their names, the rest are icons with tooltips.
    final labelled = width >= 760;
    CommandBarButton button(
      IconData icon,
      String text,
      VoidCallback? onPressed, {
      bool main = false,
    }) => CommandBarButton(
      icon: Icon(icon, size: 16),
      label: labelled && main ? Text(text) : null,
      onPressed: onPressed,
      tooltip: text,
    );

    final items = <CommandBarItem>[
      if (files.canOpen)
        button(FluentIcons.open_file, 'Open', () => files.activate(sel.first), main: true),
      if (caps.download)
        button(
          FluentIcons.cloud_download,
          'Download',
          busy ? null : files.downloadSelected,
          main: true,
        ),
      if (caps.free)
        button(
          FluentIcons.hard_drive,
          'Free Up Space',
          busy ? null : () => _free(context, files),
          main: true,
        ),
      if (caps.retry) button(FluentIcons.refresh, 'Retry', files.retrySelected, main: true),
      if (caps.keep != Keep.unavailable)
        button(
          caps.keep == Keep.off ? FluentIcons.pin : FluentIcons.pinned_solid,
          caps.keep == Keep.inherited ? 'Kept via a folder above' : 'Always keep on this computer',
          caps.keep == Keep.off || caps.keep == Keep.on
              ? () => files.setKeepSelected(caps.keep == Keep.off)
              : null,
        ),
      if (caps.count == 1)
        button(
          FluentIcons.open_in_new_window,
          'Open in Google Drive',
          caps.openDrive ? files.openSelectedInDrive : null,
        ),
      if (caps.show)
        button(FluentIcons.fabric_folder, 'Show in File Explorer', files.showSelectedInExplorer),
    ];
    return Padding(
      padding: EdgeInsets.fromLTRB(gutter, 0, gutter, 8),
      child: Surface(
        padding: const EdgeInsets.fromLTRB(14, 4, 4, 4),
        radius: 6,
        color: theme.accent.bg,
        child: Row(
          children: [
            Expanded(
              flex: 3,
              child: Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.typography.bodyStrong,
              ),
            ),
            Expanded(
              flex: narrow ? 4 : 7,
              child: CommandBar(
                // Fluent's CommandBar remembers which items it moved into the overflow menu
                // by index; a new key per set of commands keeps that from going stale.
                key: ValueKey(items.map((i) => (i as CommandBarButton).tooltip).join('|')),
                isCompact: !labelled,
                mainAxisAlignment: MainAxisAlignment.end,
                overflowBehavior: CommandBarOverflowBehavior.dynamicOverflow,
                primaryItems: items,
              ),
            ),
            Tooltip(
              message: 'Clear selection (Esc)',
              child: IconButton(
                icon: const Icon(FluentIcons.clear, size: 12),
                onPressed: files.clearSelection,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

// ---------- table ----------

double _statusWidth(bool narrow) => narrow ? 150 : 210;

class _HeaderRow extends StatelessWidget {
  const _HeaderRow({required this.files, required this.narrow, required this.gutter});

  final FilesController files;
  final bool narrow;
  final double gutter;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    Widget cell(String title, SortKey key, {double? width, bool right = false}) {
      final active = files.sort.key == key;
      final child = HoverButton(
        onPressed: () => files.setSort(Sort(key, active ? !files.sort.descending : false)),
        builder: (context, states) => Container(
          height: 32,
          alignment: right ? Alignment.centerRight : Alignment.centerLeft,
          color: states.isHovered ? theme.resources.subtleFillColorSecondary : null,
          padding: const EdgeInsets.symmetric(horizontal: 8),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                title,
                style: theme.typography.caption?.copyWith(
                  color: theme.secondaryText,
                  fontWeight: active ? FontWeight.w600 : FontWeight.w400,
                ),
              ),
              if (active) ...[
                const SizedBox(width: 4),
                Icon(
                  files.sort.descending ? FluentIcons.chevron_down : FluentIcons.chevron_up,
                  size: 9,
                  color: theme.secondaryText,
                ),
              ],
            ],
          ),
        ),
      );
      return width == null ? Expanded(child: child) : SizedBox(width: width, child: child);
    }

    return Padding(
      padding: EdgeInsetsDirectional.only(start: gutter - 8, end: gutter - 2),
      child: Column(
        children: [
          Row(
            children: [
              const SizedBox(width: 56),
              cell('Name', SortKey.name),
              cell('Status', SortKey.status, width: _statusWidth(narrow)),
              if (!narrow) cell('Size', SortKey.size, width: 90, right: true),
              if (!narrow) cell('Modified', SortKey.date, width: 130),
            ],
          ),
          const Divider(),
        ],
      ),
    );
  }
}

class _FileRow extends StatefulWidget {
  const _FileRow({
    super.key,
    required this.files,
    required this.item,
    required this.narrow,
    required this.onMenu,
    required this.onFocusList,
  });

  final FilesController files;
  final DirItem item;
  final bool narrow;
  final void Function(Offset globalPosition) onMenu;
  final VoidCallback onFocusList;

  @override
  State<_FileRow> createState() => _FileRowState();
}

class _FileRowState extends State<_FileRow> {
  bool _hover = false;

  void _tap() {
    widget.onFocusList();
    final kb = HardwareKeyboard.instance;
    final files = widget.files;
    final path = widget.item.path;
    if (kb.isShiftPressed) {
      files.selectRange(path);
    } else if (kb.isControlPressed || kb.isMetaPressed) {
      files.toggle(path);
    } else {
      files.selectOnly(path);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final files = widget.files;
    final it = widget.item;
    final e = it.entry;
    final selected = files.isSelected(it.path);
    final flat = files.flat;
    final narrow = widget.narrow;
    final now = fmt.nowCivil();
    final kept = it.isDir && files.keepStateOf(it.path) != Keep.off;
    final cloud = e.state == EntryState.cloudOnly;
    final bg = selected
        ? theme.accent.bg
        : _hover
        ? theme.resources.subtleFillColorSecondary
        : Colors.transparent;

    final parent = parentOf(e.path);
    final nameColumn = Row(
      children: [
        FileTypeIcon(name: it.name, isDir: it.isDir, dim: cloud),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Flexible(
                    child: Tooltip(
                      message: it.name,
                      child: Text(
                        it.name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(color: cloud ? theme.secondaryText : null),
                      ),
                    ),
                  ),
                  if (kept) ...[
                    const SizedBox(width: 6),
                    Tooltip(
                      message: 'Always kept on this computer',
                      child: Icon(FluentIcons.pinned_solid, size: 11, color: theme.secondaryText),
                    ),
                  ],
                ],
              ),
              if (flat)
                Text(
                  parent.isEmpty ? 'CloudDrop' : parent,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                ),
            ],
          ),
        ),
      ],
    );

    Widget statusCell;
    final stats = it.stats;
    if (stats != null && it.isDir && e.state != EntryState.error) {
      final st = fmt.folderStatus(stats);
      statusCell = Text(
        fmt.folderSummary(stats),
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: theme.typography.caption?.copyWith(
          color: st == fmt.Status.failed
              ? theme.resources.systemFillColorCritical
              : st != null
              ? theme.resources.systemFillColorCaution
              : theme.secondaryText,
        ),
      );
    } else {
      statusCell = Align(
        alignment: Alignment.centerLeft,
        child: StatusPill(fmt.Status.of(e.state), tooltip: e.error),
      );
    }

    final emptyDir = stats != null && stats.files == 0;
    final date = itemDateMs(e);

    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: _tap,
        onDoubleTap: () => files.activate(it),
        onSecondaryTapUp: (d) => widget.onMenu(d.globalPosition),
        child: Padding(
          padding: const EdgeInsets.symmetric(vertical: 1),
          child: Container(
            decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(6)),
            child: Row(
              children: [
                SizedBox(
                  width: 40,
                  child: Center(
                    child: Opacity(
                      opacity: selected || _hover ? 1 : 0.0,
                      child: Checkbox(
                        checked: selected,
                        onChanged: (_) {
                          widget.onFocusList();
                          files.toggle(it.path);
                        },
                      ),
                    ),
                  ),
                ),
                Expanded(child: nameColumn),
                SizedBox(
                  width: _statusWidth(narrow),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 8),
                    child: statusCell,
                  ),
                ),
                if (!narrow)
                  SizedBox(
                    width: 90,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                      child: Text(
                        emptyDir ? '—' : fmt.humanBytes(it.bytes),
                        textAlign: TextAlign.right,
                        maxLines: 1,
                        style: theme.typography.body?.copyWith(
                          fontFeatures: const [FontFeature.tabularFigures()],
                        ),
                      ),
                    ),
                  ),
                if (!narrow)
                  SizedBox(
                    width: 130,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 8),
                      child: Tooltip(
                        message: _uploadedTip(e, now),
                        child: Text(
                          date == null ? '—' : fmt.shortWhen(fmt.civilFromMs(date), now),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.typography.body?.copyWith(color: theme.secondaryText),
                        ),
                      ),
                    ),
                  ),
                const SizedBox(width: 6),
              ],
            ),
          ),
        ),
      ),
    );
  }

  String _uploadedTip(Entry e, fmt.Civil now) {
    final u = e.uploadedAt;
    if (u == null) return '';
    final c = fmt.civilFromMs(u);
    final when = fmt.shortWhen(c, now);
    return 'Uploaded $when${c.days == now.days ? '' : ' ${c.clock}'}'.trim();
  }
}

// ---------- empty ----------

class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.files});

  final FilesController files;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final e = files.emptyState;
    final icon = files.search.isNotEmpty
        ? FluentIcons.search
        : files.filter != Filter.all
        ? FluentIcons.filter
        : e.canAdd
        ? FluentIcons.cloud_upload
        : FluentIcons.fabric_folder;
    final content = Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (e.canAdd)
          IconBadge(icon: icon, tint: theme.accent, size: 64, iconSize: 28)
        else
          Icon(icon, size: 44, color: theme.secondaryText),
        const SizedBox(height: 14),
        Text(e.title, style: theme.typography.subtitle, textAlign: TextAlign.center),
        if (e.text.isNotEmpty) ...[
          const SizedBox(height: 6),
          ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 380),
            child: Text(
              e.text,
              textAlign: TextAlign.center,
              style: theme.typography.body?.copyWith(color: theme.secondaryText),
            ),
          ),
        ],
        if (e.canAdd) ...[
          const SizedBox(height: 16),
          FilledButton(onPressed: files.addFiles, child: const Text('Add Files…')),
        ],
      ],
    );
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: e.canAdd
            // Where files can be added, the empty folder is a drop zone.
            ? CustomPaint(
                painter: _DashedOutline(color: theme.resources.controlStrokeColorSecondary),
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 40, vertical: 32),
                  child: content,
                ),
              )
            : content,
      ),
    );
  }
}

/// A rounded, dashed outline around the empty drop zone.
class _DashedOutline extends CustomPainter {
  const _DashedOutline({required this.color});

  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 1.5;
    final outline = Path()
      ..addRRect(RRect.fromRectAndRadius(Offset.zero & size, const Radius.circular(12)));
    const dash = 7.0, gap = 5.0;
    for (final metric in outline.computeMetrics()) {
      for (var d = 0.0; d < metric.length; d += dash + gap) {
        canvas.drawPath(metric.extractPath(d, d + dash), paint);
      }
    }
  }

  @override
  bool shouldRepaint(_DashedOutline old) => old.color != color;
}
