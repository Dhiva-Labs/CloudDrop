library;

import 'package:fluent_ui/fluent_ui.dart';

import '../state/activity_controller.dart';
import '../state/app_controller.dart';
import '../state/files_controller.dart';

/// Hands the controllers to the screens without any extra package.
class AppScope extends InheritedWidget {
  const AppScope({
    super.key,
    required this.app,
    required this.files,
    required this.activity,
    required super.child,
  });

  final AppController app;
  final FilesController files;
  final ActivityController activity;

  static AppScope of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<AppScope>();
    assert(scope != null, 'No AppScope above this widget');
    return scope!;
  }

  @override
  bool updateShouldNotify(AppScope old) =>
      app != old.app || files != old.files || activity != old.activity;
}
