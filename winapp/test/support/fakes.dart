/// Hand-written fakes for the interfaces in lib/data, used by unit and widget tests.
library;

import 'dart:async';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/paths.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:clouddrop_app/data/status_source.dart';
import 'package:clouddrop_app/data/system_actions.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:path/path.dart' as p;

import 'test_db.dart';

// ---------- CLI ----------

class FakeCliProcess implements CliProcess {
  final _lines = StreamController<String>();
  final _exit = Completer<int>();
  String stderr = '';
  bool killed = false;

  @override
  Stream<String> get lines => _lines.stream;

  @override
  Future<int> get exitCode => _exit.future;

  @override
  Future<String> get stderrText async => stderr;

  void say(String line) => _lines.add(line);

  void finish(int code) {
    _lines.close();
    if (!_exit.isCompleted) _exit.complete(code);
  }

  @override
  void kill() {
    killed = true;
    finish(143);
  }
}

class FakeCliRunner implements CliRunner {
  final calls = <List<String>>[];
  final envs = <Map<String, String>?>[];
  CliResult Function(List<String> args, Map<String, String>? env) onRun = (args, env) =>
      const CliResult(0, '', '');
  final processes = <FakeCliProcess>[];
  Object? startError;

  @override
  Future<CliResult> run(
    List<String> args, {
    Map<String, String>? environment,
    Duration? timeout,
  }) async {
    calls.add(args);
    envs.add(environment);
    return onRun(args, environment);
  }

  @override
  Future<CliProcess> start(List<String> args) async {
    calls.add(args);
    envs.add(null);
    final e = startError;
    if (e != null) throw e;
    final p = FakeCliProcess();
    processes.add(p);
    return p;
  }
}

// ---------- engine ----------

class FakeLogin implements LoginSession {
  FakeLogin();
  final urlC = Completer<String>();
  final doneC = Completer<ActionResult>();
  bool cancelled = false;

  @override
  Future<String> get url => urlC.future;

  @override
  Future<ActionResult> get done => doneC.future;

  @override
  void cancel() {
    cancelled = true;
    if (!doneC.isCompleted) doneC.complete(const ActionResult(false, 'Sign-in cancelled.'));
    if (!urlC.isCompleted) urlC.completeError(StateError('Sign-in cancelled.'));
  }
}

/// Records what the UI asks the engine to do; answers come from the public fields.
class FakeEngine implements EngineApi {
  FakeEngine({this.statusValue, this.config});

  EngineStatus? statusValue;
  AppConfig? config;
  Object? statusError;
  ServiceStatus service = const ServiceStatus(running: true, startsAtSignIn: true);
  final calls = <String>[];
  final logins = <FakeLogin>[];
  ActionResult Function(String call) result = (c) => ActionResult(true, 'Done: $c');

  ActionResult _do(String call) {
    calls.add(call);
    return result(call);
  }

  @override
  Future<EngineStatus> status() async {
    final e = statusError;
    if (e != null) throw e;
    return statusValue ?? const EngineStatus.unconfigured();
  }

  @override
  Future<AppConfig?> configGet() async => config;

  @override
  Future<ActionResult> configSet(String key, String value) async => _do('config set $key $value');

  @override
  Future<ActionResult> setup({
    required String folder,
    required String driveFolder,
    String? clientJsonPath,
    String? clientId,
    String? clientSecret,
  }) async => _do(
    'setup folder=$folder drive=$driveFolder json=$clientJsonPath id=$clientId secret=$clientSecret',
  );

  @override
  LoginSession login() {
    calls.add('login');
    final l = FakeLogin();
    logins.add(l);
    return l;
  }

  @override
  Future<ActionResult> logout() async => _do('logout');

  @override
  Future<ActionResult> free(List<String> paths) async => _do('free ${paths.join(' ')}');

  @override
  Future<ActionResult> restore(List<String> paths) async => _do('restore ${paths.join(' ')}');

  @override
  Future<ActionResult> retry(List<String> paths) async => _do('retry ${paths.join(' ')}'.trim());

  @override
  Future<ActionResult> pause() async => _do('pause');

  @override
  Future<ActionResult> resume() async => _do('resume');

  @override
  Future<ActionResult> keep(List<String> paths) async => _do('keep ${paths.join(' ')}');

  @override
  Future<ActionResult> unkeep(List<String> paths) async => _do('unkeep ${paths.join(' ')}');

  @override
  Future<ServiceStatus> serviceStatus() async => service;

  @override
  Future<ActionResult> serviceStart() async => _do('service start');

  @override
  Future<ActionResult> serviceEnable() async => _do('service enable');

  @override
  Future<ActionResult> serviceDisable() async => _do('service disable');

  @override
  Future<ActionResult> driveUrl(String path) async {
    final r = _do('url $path');
    return r.ok ? const ActionResult(true, 'https://drive.google.com/drive/folders/ROOT') : r;
  }
}

// ---------- status.json and the OS ----------

class FakeStatus implements StatusSource {
  FakeStatus([this.value]);
  DaemonStatus? value;

  @override
  Future<DaemonStatus?> read() async => value;
}

class FakeSystem implements SystemActions {
  final opened = <String>[];
  final revealed = <String>[];
  final urls = <String>[];
  List<String> files = [];
  String? directory;
  String? jsonFile;

  @override
  Future<void> openPath(String path) async => opened.add(path);

  @override
  Future<void> reveal(String path) async => revealed.add(path);

  @override
  Future<void> openUrl(String url) async => urls.add(url);

  @override
  Future<List<String>> pickFiles({String? title}) async => files;

  @override
  Future<String?> pickDirectory({String? title, String? initial}) async => directory;

  @override
  Future<String?> pickJsonFile({String? initial}) async => jsonFile;

  @override
  String? get downloadsDir => '/home/me/Downloads';
}

// ---------- a whole app wired to fakes ----------

/// An [AppController] over an in-memory database and fakes, without the polling timer
/// (tests call `poll()` themselves).
class TestApp {
  TestApp({
    TestDb? db,
    FakeEngine? engine,
    String? syncDir,
    bool windows = false,
    DaemonStatus? daemon,
    EngineStatus? status,
    AppConfig? config,
    int? nowMs,
    this.cloudFiles = false,
    StateReader? reader,
  }) : syncDir = syncDir ?? (windows ? r'C:\Users\Dhiva\CloudDrop' : '/home/me/CloudDrop'),
       db = db ?? TestDb.memory(),
       engine = engine ?? FakeEngine(),
       statusSource = FakeStatus(daemon),
       system = FakeSystem() {
    this.engine.config =
        config ??
        this.engine.config ??
        AppConfig(
          syncDir: this.syncDir,
          driveFolder: 'CloudDrop',
          clientId: '123-test.apps.googleusercontent.com',
          hasClientSecret: true,
          ignore: const ['.clouddrop-*', '*.part', '*.tmp'],
        );
    this.engine.statusValue =
        status ??
        this.engine.statusValue ??
        EngineStatus(
          configured: true,
          signedIn: true,
          hasBuiltinClient: false,
          folder: this.syncDir,
          driveFolder: 'CloudDrop',
        );
    controller = AppController(
      AppServices(
        paths: windows
            ? AppPaths(
                configPath: r'C:\Users\Dhiva\AppData\Roaming\clouddrop\config.toml',
                dataDir: r'C:\Users\Dhiva\AppData\Local\clouddrop',
                defaultSyncDir: r'C:\Users\Dhiva\CloudDrop',
                cliBinary: 'clouddrop.exe',
                homeDir: r'C:\Users\Dhiva',
                context: p.windows,
              )
            : AppPaths(
                configPath: '/home/me/.config/clouddrop/config.toml',
                dataDir: '/home/me/.local/share/clouddrop',
                defaultSyncDir: '/home/me/CloudDrop',
                cliBinary: 'clouddrop',
                homeDir: '/home/me',
              ),
        engine: this.engine,
        db: reader ?? this.db.reader(),
        status: statusSource,
        system: system,
        searchDelay: Duration.zero,
        caseInsensitiveFs: false,
        cloudFilesSyncRoot: cloudFiles,
        pathExists: (path) async => onDisk.contains(path),
      ),
    );
  }

  final TestDb db;
  final FakeEngine engine;
  final FakeStatus statusSource;
  final FakeSystem system;
  final String syncDir;

  /// Windows behaviour: freed items stay on disk as placeholders (the paths in [onDisk]).
  final bool cloudFiles;

  /// Absolute paths that exist on the (fake) disk, as far as the controllers can tell.
  final Set<String> onDisk = {};
  late final AppController controller;

  Future<AppController> ready() async {
    await controller.poll(forceEngine: true);
    return controller;
  }
}
