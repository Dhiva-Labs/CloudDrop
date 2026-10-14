import 'package:flutter/widgets.dart';

import 'data/cli_runner.dart';
import 'data/engine_api.dart';
import 'data/paths.dart';
import 'data/state_reader.dart';
import 'data/status_source.dart';
import 'data/system_actions.dart';
import 'state/app_controller.dart';
import 'ui/app.dart';
import 'window.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final mica = await initWindow();

  // The same files the engine uses (docs/WINDOWS.md); CLOUDDROP_CONFIG,
  // CLOUDDROP_DATA_DIR and CLOUDDROP_BIN override them.
  final paths = AppPaths.resolve();
  final app = AppController(
    AppServices(
      paths: paths,
      engine: CliEngineApi(ProcessCliRunner(paths.cliBinary)),
      db: SqliteStateReader.file(paths.dbPath),
      status: FileStatusSource(paths.statusPath),
      system: const DesktopSystemActions(),
    ),
  )..start();

  runApp(CloudDropApp(app: app, mica: mica, onBrightness: syncMica));
}
