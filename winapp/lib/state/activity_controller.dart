/// State of the Activity screen (and Home's recent list): the newest history rows, loaded
/// when something new was logged (or the day changed) while either screen shows.
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../data/models.dart';
import '../logic/activity_text.dart';
import '../logic/fmt.dart';
import 'app_controller.dart';

class ActivityController extends ChangeNotifier {
  ActivityController(this.app) {
    app.dbVersion.addListener(_onDbChanged);
    app.pane.addListener(_onPane);
    app.connectionVersion.addListener(_onConnection);
    visible = _shows(app.pane.value);
  }

  static bool _shows(AppPane p) => p == AppPane.activity || p == AppPane.home;

  /// How many rows Home shows.
  static const int recentCount = 4;

  static const int load = 500;
  static const int pageSize = 100;

  final AppController app;

  List<Activity> items = const [];
  ActivityFilter _filter = ActivityFilter.all;
  int shown = pageSize;
  bool loaded = false;
  int? _lastId;
  int _day = 0;
  bool _visible = false;
  bool _disposed = false;

  ActivityFilter get filter => _filter;

  set filter(ActivityFilter f) {
    if (_filter == f) return;
    _filter = f;
    shown = pageSize;
    notifyListeners();
  }

  /// The loaded rows the filter lets through.
  List<Activity> get filtered => _filter == ActivityFilter.all
      ? items
      : [
          for (final a in items)
            if (_filter.matches(a.kind)) a,
        ];

  List<Activity> get visibleItems {
    final f = filtered;
    return f.length <= shown ? f : f.sublist(0, shown);
  }

  bool get hasMore => shown < filtered.length;

  /// The newest rows, whatever the filter.
  List<Activity> get recent => items.length <= recentCount ? items : items.sublist(0, recentCount);

  void showMore() {
    shown += pageSize;
    notifyListeners();
  }

  set visible(bool v) {
    if (_visible == v) return;
    _visible = v;
    if (v) refresh();
  }

  void _onPane() => visible = _shows(app.pane.value);

  void _onDbChanged() {
    if (_visible) refresh();
  }

  /// Another folder is shown: its own history, from the top.
  void _onConnection() {
    items = const [];
    loaded = false;
    _lastId = null;
    shown = pageSize;
    if (!_disposed) notifyListeners();
    // A read of the old connection may be running: read again once it's done.
    final running = _running;
    if (running != null) {
      running.whenComplete(() {
        _lastId = null;
        loaded = false;
        if (_visible) refresh();
      });
    } else if (_visible) {
      refresh();
    }
  }

  /// Reads the history if something new was logged since the last read.
  Future<void> refresh() {
    final running = _running;
    if (running != null) return running;
    if (_disposed) return Future.value();
    return _running = _read().whenComplete(() => _running = null);
  }

  Future<void>? _running;

  Future<void> _read() async {
    final version = app.connectionVersion.value;
    final db = app.db;
    final id = await db.lastActivityId();
    final today = nowCivil().days;
    if (version != app.connectionVersion.value) return;
    if (loaded && _lastId == id && _day == today) return;
    final first = _lastId == null;
    _lastId = id;
    _day = today;
    final rows = await db.recentActivity(load);
    // Another folder was selected meanwhile: these rows are not its.
    if (version != app.connectionVersion.value) return;
    items = rows;
    if (first) shown = pageSize;
    loaded = true;
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    app.dbVersion.removeListener(_onDbChanged);
    app.pane.removeListener(_onPane);
    app.connectionVersion.removeListener(_onConnection);
    super.dispose();
  }
}
