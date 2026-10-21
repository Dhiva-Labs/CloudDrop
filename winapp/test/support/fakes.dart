/// Hand-written fakes for the interfaces in lib/data, used by unit and widget tests.
library;

import 'dart:async';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/paths.dart';
import 'package:clouddrop_app/data/providers.dart';
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

  /// What `providers --json` lists.
  List<CloudProvider> providerList = const [CloudProvider.google];
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
  Future<ActionResult> configSet(String key, String value) async {
    final r = _do('config set $key $value');
    // The look is read back from the config (the other settings stay as the test set them).
    final c = config;
    if (r.ok && c != null && (key == 'theme' || key == 'accent')) {
      config = key == 'theme' ? c.copyWith(theme: value) : c.copyWith(accent: value);
    }
    return r;
  }

  @override
  Future<List<CloudProvider>> providers() async => providerList;

  /// A Google setup reads exactly as it always did; another provider adds its id, settings and
  /// secrets.
  @override
  Future<ActionResult> setup({
    required String folder,
    required String driveFolder,
    String? clientJsonPath,
    String? clientId,
    String? clientSecret,
    String? provider,
    Map<String, String> settings = const {},
    Map<String, String> secrets = const {},
  }) async {
    var call =
        'setup folder=$folder drive=$driveFolder json=$clientJsonPath id=$clientId '
        'secret=$clientSecret';
    if (provider != null) call += ' provider=$provider';
    for (final e in settings.entries) {
      call += ' --set ${e.key}=${e.value}';
    }
    for (final e in secrets.entries) {
      call += ' --secret ${e.key}=${e.value}';
    }
    return _do(call);
  }

  @override
  Future<ActionResult> check() async => _do('check');

  @override
  Future<ActionResult> resetCloud() async => _do('reset-cloud');

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
  Future<ActionResult> serviceStop() async => _do('service stop');

  @override
  Future<ActionResult> serviceEnable() async => _do('service enable');

  @override
  Future<ActionResult> serviceDisable() async => _do('service disable');

  /// A link the test's [result] gives, else a Drive folder link.
  @override
  Future<ActionResult> driveUrl(String path) async {
    final r = _do('url $path');
    if (!r.ok || r.message.startsWith('https://')) return r;
    return const ActionResult(true, 'https://drive.google.com/drive/folders/ROOT');
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
  String? file;

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
  Future<String?> pickFile({String? title}) async => file;

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
    String windowsUser = 'Dhiva',
    DaemonStatus? daemon,
    EngineStatus? status,
    AppConfig? config,
    int? nowMs,
    this.cloudFiles = false,
    StateReader? reader,
    CloudProvider? provider,
  }) : syncDir = syncDir ?? (windows ? 'C:\\Users\\$windowsUser\\CloudDrop' : '/home/me/CloudDrop'),
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
          provider: provider?.id,
          providerName: provider?.name,
        );
    // The app finds a configured provider in the CLI's list.
    if (provider != null && !this.engine.providerList.contains(provider)) {
      this.engine.providerList = [...this.engine.providerList, provider];
    }
    controller = AppController(
      AppServices(
        paths: windows
            ? AppPaths(
                configPath: 'C:\\Users\\$windowsUser\\AppData\\Roaming\\clouddrop\\config.toml',
                dataDir: 'C:\\Users\\$windowsUser\\AppData\\Local\\clouddrop',
                defaultSyncDir: 'C:\\Users\\$windowsUser\\CloudDrop',
                cliBinary: 'clouddrop.exe',
                homeDir: 'C:\\Users\\$windowsUser',
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

  /// Plays the three heartbeats before the current status (a second apart, the uploads
  /// [bytesPerSecond] behind each time), so Home has measured a speed.
  Future<void> measureSpeed(int bytesPerSecond) async {
    final last = statusSource.value!;
    final share = bytesPerSecond ~/ last.uploads.length;
    // The app has already read [last]: a moment without the engine starts the meter over.
    statusSource.value = null;
    await controller.poll();
    for (var k = 3; k >= 1; k--) {
      statusSource.value = DaemonStatus(
        pid: last.pid,
        updatedMs: last.updatedMs - k * 1000,
        phase: last.phase,
        waiting: last.waiting,
        quota: last.quota,
        uploads: [
          for (final u in last.uploads)
            Upload(path: u.path, sent: u.sent - k * share, total: u.total),
        ],
      );
      await controller.poll();
    }
    statusSource.value = last;
    await controller.poll();
  }
}
