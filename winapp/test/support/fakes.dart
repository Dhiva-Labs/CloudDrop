/// Hand-written fakes for the interfaces in lib/data, used by unit and widget tests.
library;

import 'dart:async';

import 'package:clouddrop_app/data/app_prefs.dart';
import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/connections.dart';
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

/// The last part of a path, Windows or POSIX.
String baseName(String path) => (path.contains('\\') ? p.windows : p.posix).basename(path);

/// Records what the UI asks the engine to do; answers come from the public fields.
///
/// It is the default connection's engine; [forConnection] gives another connection's, a
/// [FakeEngine] of its own (in [views]) whose calls also land in the shared [log].
class FakeEngine implements EngineApi {
  FakeEngine({this.statusValue, this.config}) : connection = Connection.defaultName, _root = null;

  FakeEngine._view(FakeEngine root, this.connection) : _root = root;

  final FakeEngine? _root;
  FakeEngine get root => _root ?? this;

  @override
  final String connection;

  EngineStatus? statusValue;
  AppConfig? config;
  Object? statusError;
  ServiceStatus service = const ServiceStatus(running: true, startsAtSignIn: true);

  /// What `providers --json` lists.
  List<CloudProvider> providerList = const [CloudProvider.google];

  /// This connection's calls.
  final calls = <String>[];

  /// Every connection's calls, as "connection: call" (on the root).
  final log = <String>[];
  final logins = <FakeLogin>[];
  ActionResult Function(String call) result = (c) => ActionResult(true, 'Done: $c');

  /// The other connections' engines, by name.
  final Map<String, FakeEngine> views = {};

  /// What `connections --json` lists; null: the default connection, once it has a config (as
  /// the CLI, which lists a connection once its config file exists).
  List<Connection>? connectionList;

  /// false: an engine from before connections (`connections --json` fails).
  bool connectionsSupported = true;

  ActionResult _do(String call) {
    calls.add(call);
    root.log.add('$connection: $call');
    return result(call);
  }

  @override
  EngineApi forConnection(String name) => view(name);

  /// The engine of connection [name] (made on first use).
  FakeEngine view(String name) {
    final r = root;
    if (name == Connection.defaultName) return r;
    return r.views[name] ??= FakeEngine._view(r, name);
  }

  /// The connections as the CLI would list them now.
  List<Connection> get listed {
    final r = root;
    final list = r.connectionList;
    if (list != null) return list;
    final folder = r.config?.syncDir ?? r.statusValue?.folder;
    if (r.config == null && !(r.statusValue?.configured ?? false)) return const [];
    return [
      Connection(
        name: Connection.defaultName,
        label: folder == null ? 'CloudDrop' : baseName(folder),
        folder: folder,
        provider: r.statusValue?.provider ?? CloudProvider.google.id,
        driveFolder: r.config?.driveFolder,
        configured: r.statusValue?.configured ?? true,
      ),
    ];
  }

  @override
  Future<List<Connection>?> connections() async =>
      root.connectionsSupported ? List.of(listed) : null;

  @override
  Future<ActionResult> addConnection(
    String name, {
    required String folder,
    required String driveFolder,
    String? clientJsonPath,
    String? clientId,
    String? clientSecret,
    String? provider,
    Map<String, String> settings = const {},
    Map<String, String> secrets = const {},
  }) async {
    final r = root;
    var call = 'connections add $name folder=$folder drive=$driveFolder';
    if (clientJsonPath != null) call += ' json=$clientJsonPath';
    if (clientId != null) call += ' id=$clientId secret=$clientSecret';
    if (provider != null) call += ' provider=$provider';
    for (final e in settings.entries) {
      call += ' --set ${e.key}=${e.value}';
    }
    for (final e in secrets.entries) {
      call += ' --secret ${e.key}=${e.value}';
    }
    final res = r._do(call);
    if (res.ok) {
      r.connectionList = [
        ...r.listed,
        Connection(
          name: name,
          label: name,
          folder: folder,
          provider: provider ?? CloudProvider.google.id,
          driveFolder: driveFolder,
          configured: true,
        ),
      ];
      final v = view(name);
      v.statusValue ??= EngineStatus(
        configured: true,
        folder: folder,
        driveFolder: driveFolder,
        provider: provider,
      );
      v.config ??= _withLook(
        AppConfig(syncDir: folder, driveFolder: driveFolder, clientId: '', hasClientSecret: false),
      );
    }
    return res;
  }

  @override
  Future<ActionResult> removeConnection(String name) async {
    final r = root;
    final res = r._do('connections remove $name');
    if (res.ok) {
      r.connectionList = [
        for (final c in r.listed)
          if (c.name != name) c,
      ];
      if (name == Connection.defaultName) {
        r.config = null;
        r.statusValue = const EngineStatus.unconfigured();
      }
    }
    return res;
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
    // It is the installation's: like the CLI, every connection gets it.
    if (r.ok && (key == 'theme' || key == 'accent')) {
      for (final e in [root, ...root.views.values]) {
        final c = e.config;
        if (c != null) {
          e.config = key == 'theme' ? c.copyWith(theme: value) : c.copyWith(accent: value);
        }
      }
    }
    return r;
  }

  /// [c] with the installation's look (as the CLI gives a new connection).
  AppConfig _withLook(AppConfig c) {
    final look = root.config;
    return look == null ? c : c.copyWith(theme: look.theme, accent: look.accent);
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

/// Another connection of a [TestApp]: its engine (a view of the app's), database and status.
class TestConnection {
  TestConnection(this.name, this.engine) : db = TestDb.memory() {
    reader = db.reader();
  }

  final String name;
  final FakeEngine engine;
  final TestDb db;
  late final StateReader reader;
  final FakeStatus status = FakeStatus();
}

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
  }) : isWindows = windows,
       syncDir = syncDir ?? (windows ? 'C:\\Users\\$windowsUser\\CloudDrop' : '/home/me/CloudDrop'),
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
        db: _reader = reader ?? this.db.reader(),
        status: statusSource,
        system: system,
        searchDelay: Duration.zero,
        caseInsensitiveFs: false,
        cloudFilesSyncRoot: cloudFiles,
        pathExists: (path) async => onDisk.contains(path),
        prefs: prefs,
        connect: _connect,
      ),
    );
  }

  final TestDb db;
  final FakeEngine engine;
  final FakeStatus statusSource;
  final FakeSystem system;
  final String syncDir;
  final bool isWindows;
  late final StateReader _reader;

  /// `app.json`, in memory.
  final MemoryAppPrefs prefs = MemoryAppPrefs();

  /// The other connections, by name (the default one is [db], [engine] and [statusSource]).
  final Map<String, TestConnection> others = {};

  ConnectionServices _connect(Connection c) {
    if (c.isDefault) return ConnectionServices(engine: engine, db: _reader, status: statusSource);
    final o = others[c.name] ??= TestConnection(c.name, engine.view(c.name));
    return ConnectionServices(engine: o.engine, db: o.reader, status: o.status);
  }

  /// The default connection as `connections --json` lists it.
  Connection get defaultConnection => engine.listed.firstWhere(
    (c) => c.isDefault,
    orElse: () => Connection(name: Connection.defaultName, label: baseName(syncDir)),
  );

  /// Adds another connection [name] uploading [folder] (by default a folder of that name next
  /// to the default one's), set up and signed in, with its own database, engine and status.
  TestConnection addConnection(
    String name, {
    String? folder,
    CloudProvider provider = CloudProvider.google,
    DaemonStatus? daemon,
    bool signedIn = true,
    String driveFolder = 'CloudDrop',
  }) {
    final ctx = isWindows ? p.windows : p.posix;
    final dir = folder ?? ctx.join(ctx.dirname(syncDir), name);
    final view = engine.view(name)
      ..statusValue = EngineStatus(
        configured: true,
        signedIn: signedIn,
        folder: dir,
        driveFolder: driveFolder,
        provider: provider.id,
        providerName: provider.name,
      )
      // The installation's look, as every connection has it.
      ..config = engine._withLook(
        AppConfig(syncDir: dir, driveFolder: driveFolder, clientId: '', hasClientSecret: false),
      );
    if (!engine.providerList.contains(provider)) {
      engine.providerList = [...engine.providerList, provider];
    }
    final o = others[name] = TestConnection(name, view)..status.value = daemon;
    final paths = controller.paths;
    engine.connectionList = [
      ...engine.listed,
      Connection(
        name: name,
        label: name,
        folder: dir,
        config: ctx.join(ctx.dirname(paths.configPath), 'connections', name, 'config.toml'),
        dataDir: ctx.join(paths.dataDir, 'connections', name),
        provider: provider.id,
        providerName: provider.name,
        driveFolder: driveFolder,
        configured: true,
      ),
    ];
    return o;
  }

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
