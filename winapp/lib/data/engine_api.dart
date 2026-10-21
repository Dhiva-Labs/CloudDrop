/// The app's side of the app <-> engine contract (docs/WINDOWS.md): everything that is
/// not a database/`status.json` read goes through the `clouddrop` CLI.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as p;

import 'cli_runner.dart';
import 'connections.dart';
import 'models.dart';
import 'providers.dart';

/// A finished command: the engine's sentence, or its error.
class ActionResult {
  const ActionResult(this.ok, this.message);
  final bool ok;
  final String message;

  @override
  String toString() => 'ActionResult($ok, $message)';
}

/// `clouddrop status --json`.
class EngineStatus {
  const EngineStatus({
    required this.configured,
    this.folder,
    this.driveFolder,
    this.signedIn = false,
    this.hasBuiltinClient = false,
    this.daemon,
    this.files = const {},
    this.cloudBytes = 0,
    this.cloudOnlyBytes = 0,
    this.errors = const [],
    this.paused = false,
    this.rootId,
    this.configError,
    this.provider,
    this.providerName,
    this.account,
  });

  /// Not set up yet: the setup assistant is due (unless [configError] says the settings file
  /// exists but can't be read: then the assistant must not run, it would overwrite the file).
  const EngineStatus.unconfigured({
    bool signedIn = false,
    bool hasBuiltinClient = false,
    String? configError,
  }) : this(
         configured: false,
         signedIn: signedIn,
         hasBuiltinClient: hasBuiltinClient,
         configError: configError,
       );

  /// The engine has a config with a Google client (setup was done).
  final bool configured;
  final String? folder;
  final String? driveFolder;
  final bool signedIn;
  final bool hasBuiltinClient;

  /// Present only while the engine is alive.
  final DaemonStatus? daemon;

  /// File counts per state (`synced`, `pending`, `uploading`, `cloud_only`, `error`).
  final Map<String, int> files;
  final int cloudBytes;
  final int cloudOnlyBytes;
  final List<({String path, String? error})> errors;

  /// Uploading is paused by the user.
  final bool paused;

  /// Drive id of the top CloudDrop folder, once it exists.
  final String? rootId;

  /// The settings file exists but can't be read (a TOML error, say). Only ever set together with
  /// `configured: false`.
  final String? configError;

  /// The configured cloud's id (`gdrive`, `onedrive`, …); null before setup or from an engine
  /// that predates the field (Google Drive, then).
  final String? provider;

  /// Its display name ("OneDrive").
  final String? providerName;

  /// Who is signed in, where the engine can say ("me@example.com").
  final String? account;

  bool get ready => configured && signedIn;

  factory EngineStatus.fromJson(Map<String, dynamic> j) {
    final files = <String, int>{};
    final f = j['files'];
    if (f is Map) {
      f.forEach((k, v) {
        if (v is num) files['$k'] = v.toInt();
      });
    }
    final errors = <({String path, String? error})>[];
    final e = j['errors'];
    if (e is List) {
      for (final x in e) {
        if (x is Map) {
          errors.add((path: '${x['path']}', error: x['error'] as String?));
        }
      }
    }
    return EngineStatus(
      // Before setup the engine prints just {configured: false, signed_in, has_builtin_client}.
      configured: j['configured'] != false,
      paused: j['paused'] == true,
      rootId: j['root_id'] as String?,
      configError: j['config_error'] as String?,
      folder: j['folder'] as String?,
      driveFolder: j['drive_folder'] as String?,
      signedIn: j['signed_in'] == true,
      hasBuiltinClient: j['has_builtin_client'] == true,
      daemon: DaemonStatus.fromJson(j['daemon']),
      files: files,
      cloudBytes: (j['cloud_bytes'] as num?)?.toInt() ?? 0,
      cloudOnlyBytes: (j['cloud_only_bytes'] as num?)?.toInt() ?? 0,
      errors: errors,
      provider: j['provider'] as String?,
      providerName: j['provider_name'] as String?,
      account: j['account'] is String && (j['account'] as String).isNotEmpty
          ? j['account'] as String
          : null,
    );
  }
}

/// `clouddrop config get --json` (the client secret is withheld by the engine).
class AppConfig {
  const AppConfig({
    required this.syncDir,
    required this.driveFolder,
    required this.clientId,
    required this.hasClientSecret,
    this.parallelUploads = 2,
    this.settleSecs = 2,
    this.notifications = true,
    this.ignore = const [],
    this.autoFreeDays,
    this.keepLocal = const [],
    this.sections = const {},
  });

  final String syncDir;
  final String driveFolder;
  final String clientId;
  final bool hasClientSecret;
  final int parallelUploads;
  final int settleSecs;
  final bool notifications;
  final List<String> ignore;

  /// null = never, 0 = right after upload.
  final int? autoFreeDays;
  final List<String> keepLocal;

  /// Providers' own settings by provider id (`[webdav] url = …`), to prefill the setup
  /// assistant; secrets are never among them.
  final Map<String, Map<String, String>> sections;

  /// A client is configured (a built-in one is written to the config by setup).
  bool get hasClient => clientId.isNotEmpty && hasClientSecret;

  factory AppConfig.fromJson(Map<String, dynamic> j) => AppConfig(
    syncDir: j['sync_dir'] as String? ?? '',
    driveFolder: j['drive_folder'] as String? ?? 'CloudDrop',
    clientId: j['client_id'] as String? ?? '',
    hasClientSecret: j['client_secret'] == true,
    parallelUploads: (j['parallel_uploads'] as num?)?.toInt() ?? 2,
    settleSecs: (j['settle_secs'] as num?)?.toInt() ?? 2,
    notifications: j['notifications'] as bool? ?? true,
    ignore: [for (final x in (j['ignore'] as List? ?? const [])) '$x'],
    autoFreeDays: (j['auto_free_days'] as num?)?.toInt(),
    keepLocal: [for (final x in (j['keep_local'] as List? ?? const [])) '$x'],
    sections: {
      for (final e in j.entries)
        if (e.value is Map && e.key != 'endpoints')
          e.key: {
            for (final f in (e.value as Map).entries)
              if (f.value is String || f.value is num || f.value is bool) '${f.key}': '${f.value}',
          },
    },
  );

  AppConfig copyWith({
    int? parallelUploads,
    int? settleSecs,
    bool? notifications,
    List<String>? ignore,
    Object? autoFreeDays = _keep,
    List<String>? keepLocal,
  }) => AppConfig(
    syncDir: syncDir,
    driveFolder: driveFolder,
    clientId: clientId,
    hasClientSecret: hasClientSecret,
    parallelUploads: parallelUploads ?? this.parallelUploads,
    settleSecs: settleSecs ?? this.settleSecs,
    notifications: notifications ?? this.notifications,
    ignore: ignore ?? this.ignore,
    autoFreeDays: identical(autoFreeDays, _keep) ? this.autoFreeDays : autoFreeDays as int?,
    keepLocal: keepLocal ?? this.keepLocal,
    sections: sections,
  );

  @override
  bool operator ==(Object other) =>
      other is AppConfig &&
      other.syncDir == syncDir &&
      other.driveFolder == driveFolder &&
      other.clientId == clientId &&
      other.hasClientSecret == hasClientSecret &&
      other.parallelUploads == parallelUploads &&
      other.settleSecs == settleSecs &&
      other.notifications == notifications &&
      _listEq(other.ignore, ignore) &&
      other.autoFreeDays == autoFreeDays &&
      _listEq(other.keepLocal, keepLocal);

  @override
  int get hashCode => Object.hash(
    syncDir,
    driveFolder,
    clientId,
    parallelUploads,
    settleSecs,
    notifications,
    Object.hashAll(ignore),
    autoFreeDays,
    Object.hashAll(keepLocal),
  );
}

const Object _keep = Object();

bool _listEq(List<String> a, List<String> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

/// `clouddrop service status --json`: `{"running": bool, "at_login": bool}`.
class ServiceStatus {
  const ServiceStatus({required this.running, required this.startsAtSignIn});
  final bool running;
  final bool startsAtSignIn;

  factory ServiceStatus.fromJson(Map<String, dynamic> j) =>
      ServiceStatus(running: j['running'] == true, startsAtSignIn: j['at_login'] == true);

  @override
  bool operator ==(Object other) =>
      other is ServiceStatus && other.running == running && other.startsAtSignIn == startsAtSignIn;

  @override
  int get hashCode => Object.hash(running, startsAtSignIn);
}

/// A browser sign-in in progress (`clouddrop login --no-browser`).
abstract class LoginSession {
  /// The consent link; fails if the process ends before printing it.
  Future<String> get url;

  /// Completes when the process exits: ok = signed in.
  Future<ActionResult> get done;

  /// Stops waiting for the browser.
  void cancel();
}

/// What the UI needs from the engine. [CliEngineApi] is the real one; tests inject fakes.
///
/// Each instance works on one connection ([connection], passed as `--connection` to every
/// command that concerns one); [forConnection] gives the same engine for another. The
/// installation-wide commands (`connections`, `providers`, `service`) ignore it.
abstract class EngineApi {
  /// The connection the commands work on; null for an engine from before connections (it has
  /// just the one and takes no `--connection`).
  String? get connection;

  /// The same engine, working on the connection [name].
  EngineApi forConnection(String name);

  /// Every connection (`connections --json`): the default one first, then by name; empty before
  /// the first setup. null when the engine predates connections or can't be asked.
  Future<List<Connection>?> connections();

  /// `connections add NAME`: sets up a new connection uploading [folder] (no sign-in yet: that
  /// is [forConnection]`(name).login()` or `.check()`). The arguments are [setup]'s.
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
  });

  /// `connections remove NAME`: stops uploading it and forgets its settings and state. Nothing
  /// in its folder or in the cloud is deleted.
  Future<ActionResult> removeConnection(String name);

  /// Overall state (`status --json`, which also works before setup). Throws [CliUnavailable]
  /// when the CLI can't be run, [StateError] when it ran and complained.
  Future<EngineStatus> status();

  /// The settings, or null when there is no config yet.
  Future<AppConfig?> configGet();

  /// `config set KEY VALUE`, applied live by the engine.
  Future<ActionResult> configSet(String key, String value);

  /// Every cloud CloudDrop can upload to (`providers --json`); just Google Drive from an
  /// engine that can't list them.
  Future<List<CloudProvider>> providers();

  /// `setup --no-login`: writes the config and creates the folder. A key given as
  /// [clientId]/[clientSecret] reaches the CLI through a temporary JSON file. [provider] picks
  /// another cloud than the configured one (`--provider`), with its settings (`--set`) and
  /// secrets (`--secret`).
  Future<ActionResult> setup({
    required String folder,
    required String driveFolder,
    String? clientJsonPath,
    String? clientId,
    String? clientSecret,
    String? provider,
    Map<String, String> settings = const {},
    Map<String, String> secrets = const {},
  });

  /// The provider's browser sign-in (`login --no-browser`).
  LoginSession login();

  /// `clouddrop check`: connects with the saved settings; ok = it works.
  Future<ActionResult> check();

  /// `clouddrop logout`: stops the service and forgets the sign-in.
  Future<ActionResult> logout();

  /// `reset-cloud --yes`: forgets every cloud copy and uploads everything again (after
  /// switching providers).
  Future<ActionResult> resetCloud();

  Future<ActionResult> free(List<String> paths);
  Future<ActionResult> restore(List<String> paths);

  /// Re-queues failed uploads under [paths] (all of them when empty).
  Future<ActionResult> retry(List<String> paths);
  Future<ActionResult> pause();
  Future<ActionResult> resume();
  Future<ActionResult> keep(List<String> paths);
  Future<ActionResult> unkeep(List<String> paths);

  Future<ServiceStatus> serviceStatus();
  Future<ActionResult> serviceStart();
  Future<ActionResult> serviceStop();
  Future<ActionResult> serviceEnable();
  Future<ActionResult> serviceDisable();

  /// The web link of [path] in the cloud (an absolute path inside the sync folder).
  Future<ActionResult> driveUrl(String path);
}

/// Talks to the engine by running `clouddrop`.
class CliEngineApi implements EngineApi {
  CliEngineApi(this.runner, {this.connection});

  final CliRunner runner;

  @override
  final String? connection;

  @override
  EngineApi forConnection(String name) =>
      name == connection ? this : CliEngineApi(runner, connection: name);

  /// [args] for this connection: `--connection NAME` first, so the CLI never picks one from the
  /// working directory or a path.
  List<String> _on(List<String> args) {
    final c = connection;
    return c == null ? args : ['--connection', c, ...args];
  }

  /// Runs an installation-wide command as is.
  Future<ActionResult> _doAll(List<String> args) async {
    try {
      final r = await runner.run(args);
      return ActionResult(r.ok, r.message);
    } on CliUnavailable catch (e) {
      return ActionResult(false, e.toString());
    }
  }

  /// Runs a command on this connection.
  Future<ActionResult> _do(List<String> args) => _doAll(_on(args));

  @override
  Future<List<Connection>?> connections() async {
    try {
      final r = await runner.run(['connections', '--json'], timeout: const Duration(seconds: 30));
      if (!r.ok) return null;
      return parseConnections(r.stdout);
    } on CliUnavailable {
      return null;
    } on FormatException {
      return null;
    }
  }

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
  }) => _withClientJson(
    clientJsonPath,
    clientId,
    clientSecret,
    (json) => _doAll([
      'connections',
      'add',
      name,
      ..._setupArgs(folder, driveFolder, json, provider, settings, secrets),
    ]),
  );

  @override
  Future<ActionResult> removeConnection(String name) => _doAll(['connections', 'remove', name]);

  @override
  Future<EngineStatus> status() async {
    final r = await runner.run(_on(['status', '--json']), timeout: const Duration(seconds: 30));
    if (!r.ok) throw StateError(r.errorMessage);
    try {
      final j = jsonDecode(r.stdout);
      if (j is Map<String, dynamic>) return EngineStatus.fromJson(j);
    } on FormatException {
      // Fall through.
    }
    throw StateError('clouddrop status printed something unexpected');
  }

  @override
  Future<AppConfig?> configGet() async {
    final r = await runner.run(
      _on(['config', 'get', '--json']),
      timeout: const Duration(seconds: 30),
    );
    if (!r.ok) return null;
    try {
      final j = jsonDecode(r.stdout);
      return j is Map<String, dynamic> ? AppConfig.fromJson(j) : null;
    } on FormatException {
      return null;
    }
  }

  @override
  Future<ActionResult> configSet(String key, String value) => _do(['config', 'set', key, value]);

  @override
  Future<List<CloudProvider>> providers() async {
    try {
      final r = await runner.run(['providers', '--json'], timeout: const Duration(seconds: 30));
      if (r.ok) {
        final list = parseProviders(r.stdout);
        if (list.isNotEmpty) return list;
      }
    } on CliUnavailable {
      // Fall through.
    } on FormatException {
      // Fall through.
    }
    return const [CloudProvider.google];
  }

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
  }) => _withClientJson(
    clientJsonPath,
    clientId,
    clientSecret,
    (json) => _do([
      'setup',
      '--no-login',
      ..._setupArgs(folder, driveFolder, json, provider, settings, secrets),
    ]),
  );

  /// What `setup` and `connections add` take.
  List<String> _setupArgs(
    String folder,
    String driveFolder,
    String? clientJsonPath,
    String? provider,
    Map<String, String> settings,
    Map<String, String> secrets,
  ) => [
    '--folder=$folder',
    '--drive-folder=$driveFolder',
    if (clientJsonPath != null) '--client-json=$clientJsonPath',
    if (provider != null) '--provider=$provider',
    for (final e in settings.entries) ...['--set', '${e.key}=${e.value}'],
    for (final e in secrets.entries) ...['--secret', '${e.key}=${e.value}'],
  ];

  /// Runs [body] with the Google key as a client JSON file: [clientJsonPath] as is, or a key
  /// given as [clientId]/[clientSecret] written to a temporary file (never on the command line).
  Future<ActionResult> _withClientJson(
    String? clientJsonPath,
    String? clientId,
    String? clientSecret,
    Future<ActionResult> Function(String? jsonPath) body,
  ) async {
    Directory? tmp;
    try {
      var jsonPath = clientJsonPath;
      if (jsonPath == null && clientId != null && clientSecret != null) {
        tmp = await Directory.systemTemp.createTemp('clouddrop-key');
        final f = File(p.join(tmp.path, 'client.json'));
        await f.writeAsString(
          jsonEncode({
            'installed': {'client_id': clientId.trim(), 'client_secret': clientSecret.trim()},
          }),
        );
        jsonPath = f.path;
      }
      return await body(jsonPath);
    } finally {
      if (tmp != null) {
        try {
          await tmp.delete(recursive: true);
        } catch (_) {}
      }
    }
  }

  @override
  LoginSession login() => _CliLogin(runner, _on(['login', '--no-browser']));

  @override
  Future<ActionResult> check() => _do(['check']);

  @override
  Future<ActionResult> logout() => _do(['logout']);

  @override
  Future<ActionResult> resetCloud() => _do(['reset-cloud', '--yes']);

  @override
  Future<ActionResult> free(List<String> paths) => _do(['free', ...paths]);

  @override
  Future<ActionResult> restore(List<String> paths) => _do(['restore', ...paths]);

  @override
  Future<ActionResult> retry(List<String> paths) => _do(['retry', ...paths]);

  @override
  Future<ActionResult> pause() => _do(['pause']);

  @override
  Future<ActionResult> resume() => _do(['resume']);

  @override
  Future<ActionResult> keep(List<String> paths) => _do(['keep', ...paths]);

  @override
  Future<ActionResult> unkeep(List<String> paths) => _do(['unkeep', ...paths]);

  @override
  Future<ServiceStatus> serviceStatus() async {
    const unknown = ServiceStatus(running: false, startsAtSignIn: false);
    try {
      final r = await runner.run(['service', 'status', '--json']);
      if (!r.ok) return unknown;
      final j = jsonDecode(r.stdout);
      return j is Map<String, dynamic> ? ServiceStatus.fromJson(j) : unknown;
    } on CliUnavailable {
      return unknown;
    } on FormatException {
      return unknown;
    }
  }

  @override
  Future<ActionResult> serviceStart() => _doAll(['service', 'start']);

  @override
  Future<ActionResult> serviceStop() => _doAll(['service', 'stop']);

  @override
  Future<ActionResult> serviceEnable() => _doAll(['service', 'enable']);

  @override
  Future<ActionResult> serviceDisable() => _doAll(['service', 'disable']);

  @override
  Future<ActionResult> driveUrl(String path) => _do(['url', path]);
}

final RegExp _urlPattern = RegExp(r'https?://\S+');

/// Extracts the sign-in link from a line the CLI printed, if it holds one.
String? extractUrl(String line) => _urlPattern.firstMatch(line)?.group(0);

class _CliLogin implements LoginSession {
  _CliLogin(this._runner, this._args) {
    // Nobody may ask for the link (a cancelled sign-in); that is not an error.
    _url.future.ignore();
    _start();
  }

  final CliRunner _runner;
  final List<String> _args;
  final _url = Completer<String>();
  final _done = Completer<ActionResult>();
  CliProcess? _proc;
  bool _cancelled = false;

  Future<void> _start() async {
    final CliProcess proc;
    try {
      proc = await _runner.start(_args);
    } on CliUnavailable catch (e) {
      _fail(e.toString());
      return;
    }
    if (_cancelled) {
      proc.kill();
    }
    _proc = proc;
    proc.lines.listen((line) {
      final u = extractUrl(line);
      if (u != null && !_url.isCompleted) _url.complete(u);
    });
    final code = await proc.exitCode;
    final err = await proc.stderrText;
    if (code == 0) {
      if (!_url.isCompleted) _url.completeError(StateError('signed in without a link'));
      _done.complete(const ActionResult(true, 'Signed in.'));
    } else {
      _fail(_cancelled ? 'Sign-in cancelled.' : CliResult(code, '', err).errorMessage);
    }
  }

  void _fail(String message) {
    if (!_url.isCompleted) _url.completeError(StateError(message));
    if (!_done.isCompleted) _done.complete(ActionResult(false, message));
  }

  @override
  Future<String> get url => _url.future;

  @override
  Future<ActionResult> get done => _done.future;

  @override
  void cancel() {
    _cancelled = true;
    _proc?.kill();
  }
}
