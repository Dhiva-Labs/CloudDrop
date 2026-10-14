/// State of the Files screen: where you are, the filter and search, the sorted rows and
/// the selection (`gui/src/files.rs`). Rows are reloaded when the database changes, and the
/// selection survives for every row that stays.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../data/models.dart';
import '../logic/files_model.dart';
import '../logic/fmt.dart' as fmt;
import 'app_controller.dart';

class FilesController extends ChangeNotifier {
  FilesController(this.app) {
    app.dbVersion.addListener(_onDbChanged);
    app.addListener(_onAppChanged);
    app.filesNav.addListener(_onNav);
    app.pane.addListener(_onPane);
    // The screen may already be showing (a restored session, tests).
    visible = app.pane.value == AppPane.files;
  }

  final AppController app;

  // ---------- view state ----------

  String _dir = '';
  String _search = '';
  Filter _filter = Filter.all;
  Sort _sort = const Sort();

  String get dir => _dir;
  String get search => _search;
  Filter get filter => _filter;
  Sort get sort => _sort;

  /// The text in the search box (may be ahead of [search] while the debounce runs).
  String searchText = '';

  /// What the last query returned, before sorting.
  List<DirItem> _raw = const [];
  List<DirItem> items = const [];
  FolderStats? stats;
  bool truncated = false;
  bool loaded = false;
  String? error;

  /// Entry path the list should scroll to (consumed by the view).
  String? scrollTarget;

  /// Bumped when the view should jump back to the top (new place, filter or search).
  int resetCount = 0;

  final Set<String> _selected = {};
  String? _anchor;

  bool _dirty = false;
  bool _stale = true;
  bool _visible = false;
  bool _disposed = false;
  Timer? _debounce;
  String? _pendingSelect;

  Request get request => Request(_search.isEmpty ? DirScope(_dir) : SearchScope(_search), _filter);

  /// Rows come from several folders, so show where each one lives.
  bool get flat => request.isFlat;

  // ---------- selection ----------

  Set<String> get selectedPaths => Set.unmodifiable(_selected);

  List<DirItem> get selectedItems => [
    for (final i in items)
      if (_selected.contains(i.path)) i,
  ];

  bool isSelected(String path) => _selected.contains(path);

  Keep keepStateOf(String path) => keepStateFor(app.config?.keepLocal ?? const [], path);

  Caps get caps => capsFor(selectedItems, keepStateOf);

  // ---------- Windows placeholders ----------

  /// Whether a selected cloud-only file is still on disk as a Cloud Files placeholder (so it
  /// can simply be opened). Only asked for the selection, and only where such placeholders exist.
  final Map<String, bool> _onDisk = {};

  bool isPlaceholder(String path) => _onDisk[path] ?? false;

  /// "Open" is offered for one selected file that is on this computer, or whose placeholder is.
  bool get canOpen {
    final sel = selectedItems;
    if (sel.length != 1 || sel.first.isDir) return false;
    final it = sel.first;
    return it.entry.state != EntryState.cloudOnly || isPlaceholder(it.path);
  }

  void _selectionChanged() {
    notifyListeners();
    _probePlaceholders();
  }

  Future<void> _probePlaceholders() async {
    if (!app.services.hasPlaceholders) return;
    final todo = [
      for (final it in selectedItems)
        if (!it.isDir && it.entry.state == EntryState.cloudOnly && !_onDisk.containsKey(it.path))
          it.path,
    ];
    if (todo.isEmpty) return;
    var changed = false;
    for (final path in todo) {
      final here = await app.isPlaceholder(path);
      if (_disposed) return;
      _onDisk[path] = here;
      changed |= here;
    }
    if (changed) notifyListeners();
  }

  void selectOnly(String path) {
    _selected
      ..clear()
      ..add(path);
    _anchor = path;
    _selectionChanged();
  }

  void toggle(String path) {
    if (!_selected.remove(path)) _selected.add(path);
    _anchor = path;
    _selectionChanged();
  }

  /// Shift-click: everything between the anchor and [path].
  void selectRange(String path) {
    final a = items.indexWhere((i) => i.path == (_anchor ?? path));
    final b = items.indexWhere((i) => i.path == path);
    if (a < 0 || b < 0) return selectOnly(path);
    final (lo, hi) = a < b ? (a, b) : (b, a);
    _selected
      ..clear()
      ..addAll(items.sublist(lo, hi + 1).map((i) => i.path));
    _selectionChanged();
  }

  void selectAll() {
    _selected
      ..clear()
      ..addAll(items.map((i) => i.path));
    _selectionChanged();
  }

  void clearSelection() {
    if (_selected.isEmpty) return;
    _selected.clear();
    _selectionChanged();
  }

  // ---------- navigation ----------

  /// The Files screen is on screen (queries are skipped while hidden).
  set visible(bool v) {
    if (_visible == v) return;
    _visible = v;
    if (v && _stale) reload();
  }

  /// Jumps to a folder (optionally with a filter), clearing any search.
  void show(String dir, {Filter filter = Filter.all, String? select}) {
    _debounce?.cancel();
    _dir = dir;
    _search = '';
    searchText = '';
    _filter = filter;
    _pendingSelect = select;
    _resetView();
    reload();
  }

  void openDir(String dir) => show(dir);

  void goBack() {
    if (_search.isNotEmpty) {
      show(_dir, filter: _filter);
    } else if (_dir.isNotEmpty) {
      show(parentOf(_dir), filter: _filter, select: _dir);
    } else if (_filter != Filter.all) {
      show('');
    }
  }

  bool get canGoBack => _search.isNotEmpty || _dir.isNotEmpty || _filter != Filter.all;

  /// Called on every key press in the search box.
  void searchChanged(String text) {
    searchText = text;
    notifyListeners(); // the clear button follows the box at once
    _debounce?.cancel();
    final delay = app.services.searchDelay;
    if (delay == Duration.zero) {
      _applySearch(text);
    } else {
      _debounce = Timer(delay, () => _applySearch(text));
    }
  }

  void _applySearch(String text) {
    final t = text.trim();
    if (t == _search) return;
    _search = t;
    _resetView();
    reload();
  }

  void setFilter(Filter f) {
    if (f == _filter) return;
    _filter = f;
    _resetView();
    reload();
  }

  void setSort(Sort s) {
    if (s == _sort) return;
    _sort = s;
    items = sortItems(_raw, s);
    notifyListeners();
  }

  void _resetView() {
    _selected.clear();
    _anchor = null;
    resetCount++;
  }

  // ---------- loading ----------

  void _onDbChanged() {
    _stale = true;
    if (_visible) reload();
  }

  void _onAppChanged() {
    // The pinned-folder list lives in the config: repaint the pins.
    if (!_disposed) notifyListeners();
  }

  void _onNav() {
    final nav = app.filesNav.value;
    if (nav == null) return;
    show(nav.dir, filter: nav.filter, select: nav.select);
  }

  void _onPane() => visible = app.pane.value == AppPane.files;

  /// Loads the rows for the current request. Calls that arrive while a load is running
  /// share it: it runs once more afterwards, so the last request always wins.
  Future<void> reload() {
    if (_disposed) return Future.value();
    final running = _loading;
    if (running != null) {
      _dirty = true;
      return running;
    }
    return _loading = _loadLoop();
  }

  Future<void>? _loading;

  Future<void> _loadLoop() async {
    try {
      do {
        _dirty = false;
        _stale = false;
        final req = request;
        try {
          final listing = await app.services.db.browse(req);
          if (_disposed) return;
          if (req == request) {
            _apply(req, listing);
          } else {
            _dirty = true;
          }
        } catch (e) {
          error = "Couldn't read the file list: $e";
          loaded = true;
          if (!_disposed) notifyListeners();
        }
      } while (_dirty && !_disposed);
    } finally {
      _loading = null;
    }
  }

  void _apply(Request req, Listing l) {
    if (req.scope is DirScope) _dir = l.dir;
    _raw = l.items;
    items = sortItems(l.items, _sort);
    stats = l.stats;
    truncated = l.truncated;
    error = null;
    loaded = true;
    // Selection stays for every row that stays.
    final present = {for (final i in items) i.path};
    _selected.removeWhere((p) => !present.contains(p));
    _onDisk.clear(); // freed or restored since: ask again
    final pick = _pendingSelect;
    if (pick != null) {
      _pendingSelect = null;
      if (present.contains(pick)) {
        _selected
          ..clear()
          ..add(pick);
        _anchor = pick;
        scrollTarget = pick;
      }
    }
    _selectionChanged();
  }

  // ---------- summary texts ----------

  /// The line under the toolbar.
  String get summary {
    if (_search.isNotEmpty || _filter != Filter.all) {
      final total = _raw.fold<int>(0, (s, i) => s + i.bytes);
      var s = items.isEmpty
          ? ''
          : '${fmt.items(items.length)} · ${fmt.humanBytes(total < 0 ? 0 : total)}';
      if (truncated) {
        s += ' · only the first ${items.length} are shown, narrow it down with search';
      }
      return s;
    }
    final st = stats;
    if (st != null && st.files > 0) {
      var s = '${fmt.files(st.files)} · ${fmt.humanBytes(st.bytes < 0 ? 0 : st.bytes)}';
      if (st.cloudOnly > 0) s += ' · ${fmt.groupDigits(st.cloudOnly)} only in Google Drive';
      return s;
    }
    return '';
  }

  /// Icon-less title/description for the empty list.
  ({String title, String text, bool canAdd}) get emptyState {
    if (_search.isNotEmpty) {
      return (
        title: 'No Results',
        text: 'Nothing in CloudDrop is named like “$_search”.',
        canAdd: false,
      );
    }
    if (_filter != Filter.all) {
      final (t, d) = emptyText(_filter);
      return (title: t, text: d, canAdd: false);
    }
    if (_dir.isEmpty) {
      return (
        title: 'CloudDrop Is Empty',
        text: 'Drop files here or click Add Files to start uploading to Google Drive.',
        canAdd: true,
      );
    }
    return (
      title: 'This Folder Is Empty',
      text: 'Drop files here to add them to this folder.',
      canAdd: true,
    );
  }

  // ---------- actions on the selection ----------

  String what(List<DirItem> sel) => sel.length == 1 ? '“${sel.first.name}”' : fmt.items(sel.length);

  Future<void> downloadSelected() async {
    final sel = selectedItems;
    if (sel.isEmpty) return;
    await app.restore([for (final i in sel) i.path], what(sel));
    await reload();
  }

  /// Frees the selection; the caller confirmed.
  Future<void> freeSelected() async {
    final sel = selectedItems;
    if (sel.isEmpty) return;
    await app.free([for (final i in sel) i.path]);
    await reload();
  }

  Future<void> retrySelected() async {
    final sel = selectedItems;
    if (sel.isEmpty) return;
    await app.retry([for (final i in sel) i.path]);
    await reload();
  }

  Future<void> retryAllHere() async {
    await app.retry([_dir]);
    await reload();
  }

  Future<void> openSelectedInDrive() async {
    final sel = selectedItems;
    if (sel.length == 1) await app.openInDrive(sel.first.entry);
  }

  void showSelectedInExplorer() {
    final sel = selectedItems;
    if (sel.isNotEmpty) app.showInExplorer(sel.first.path);
  }

  Future<void> setKeepSelected(bool on) async {
    final sel = selectedItems;
    if (sel.length != 1 || !sel.first.isDir) return;
    await app.setKeep(sel.first.path, on);
  }

  /// Double-click / Enter on a row. A cloud-only file opens when its placeholder is on disk
  /// (Windows: Explorer downloads it on demand); otherwise it offers the download.
  Future<void> activate(DirItem it) async {
    if (it.isDir) {
      openDir(it.path);
    } else if (it.entry.state != EntryState.cloudOnly) {
      app.openLocal(it.path);
    } else if (await app.isPlaceholder(it.path)) {
      app.openLocal(it.path);
    } else {
      final path = it.path;
      app.sayWithAction('“${it.name}” is only in Google Drive', 'Download', () {
        app.restore([path], '“${it.name}”').then((_) => reload());
      });
    }
  }

  Future<void> addFiles() => app.addFiles(_dir);

  Future<void> dropped(List<String> paths) => app.copyIn(_dir, paths);

  @override
  void dispose() {
    _disposed = true;
    _debounce?.cancel();
    app.dbVersion.removeListener(_onDbChanged);
    app.removeListener(_onAppChanged);
    app.filesNav.removeListener(_onNav);
    app.pane.removeListener(_onPane);
    super.dispose();
  }
}
