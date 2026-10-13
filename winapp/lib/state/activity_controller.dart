/// State of the Activity screen: the newest history rows, loaded when something new was
/// logged (or the day changed).
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../data/models.dart';
import '../logic/fmt.dart';
import 'app_controller.dart';

class ActivityController extends ChangeNotifier {
  ActivityController(this.app) {
    app.dbVersion.addListener(_onDbChanged);
    app.pane.addListener(_onPane);
    visible = app.pane.value == AppPane.activity;
  }

  static const int load = 500;
  static const int pageSize = 100;

  final AppController app;

  List<Activity> items = const [];
  int shown = pageSize;
  bool loaded = false;
  int? _lastId;
  int _day = 0;
  bool _visible = false;
  bool _disposed = false;

  List<Activity> get visibleItems => items.length <= shown ? items : items.sublist(0, shown);

  bool get hasMore => shown < items.length;

  void showMore() {
    shown += pageSize;
    notifyListeners();
  }

  set visible(bool v) {
    if (_visible == v) return;
    _visible = v;
    if (v) refresh();
  }

  void _onPane() => visible = app.pane.value == AppPane.activity;

  void _onDbChanged() {
    if (_visible) refresh();
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
    final db = app.services.db;
    final id = await db.lastActivityId();
    final today = nowCivil().days;
    if (loaded && _lastId == id && _day == today) return;
    final first = _lastId == null;
    _lastId = id;
    _day = today;
    items = await db.recentActivity(load);
    if (first) shown = pageSize;
    loaded = true;
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    app.dbVersion.removeListener(_onDbChanged);
    app.pane.removeListener(_onPane);
    super.dispose();
  }
}
