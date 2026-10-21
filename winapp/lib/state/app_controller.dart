/// The app's shared state: the once-a-second snapshot of the engine, the settings, and
/// every action the screens can run. Plain `ChangeNotifier`; all I/O goes through the
/// interfaces in `lib/data`, so tests inject fakes.
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart' hide Summary;

import '../data/app_prefs.dart';
import '../data/cli_runner.dart';
import '../data/connections.dart';
import '../data/engine_api.dart';
import '../data/fs_ops.dart';
import '../data/models.dart';
import '../data/paths.dart';
import '../data/providers.dart';
import '../data/state_reader.dart';
import '../data/status_source.dart';
import '../data/system_actions.dart';
import '../logic/rate.dart';
import '../logic/snapshot.dart';

/// What the controller reads and runs for one connection.
class ConnectionServices {
  const ConnectionServices({required this.engine, required this.db, required this.status});

  /// The CLI, with `--connection` set to this connection.
  final EngineApi engine;

  /// Its `state.db` and `status.json`.
  final StateReader db;
  final StatusSource status;
}

/// Everything the controller talks to.
class AppServices {
  AppServices({
    required this.paths,
    required this.engine,
    required this.db,
    required this.status,
    required this.system,
    this.tick = const Duration(seconds: 1),
    this.engineEvery = 5,
    this.searchDelay = const Duration(milliseconds: 250),
    this.caseInsensitiveFs,
    this.cloudFilesSyncRoot,
    this.pathExists = _pathExists,
    this._prefs,
    this.connect,
  });

  final AppPaths paths;

  /// The CLI for what concerns the whole installation (`connections`, `providers`, `service`),
  /// and the one each connection's is made from ([EngineApi.forConnection]).
  final EngineApi engine;

  /// The default connection's database and status (with an engine from before connections,
  /// the only ones).
  final StateReader db;
  final StatusSource status;
  final SystemActions system;

  /// What the apps remember (`app.json`); defaults to the file in the data directory.
  AppPrefs get prefs => _prefs ??= FileAppPrefs(paths.appJsonPath);
  AppPrefs? _prefs;

  /// Opens a connection's services (swapped in tests); see [open].
  final ConnectionServices Function(Connection c)? connect;

  /// The engine, database and status of connection [c]: the default connection's are [db] and
  /// [status]; another's are in its data directory.
  ConnectionServices open(Connection c) {
    final f = connect;
    if (f != null) return f(c);
    final engine = this.engine.forConnection(c.name);
    if (c.isDefault) return ConnectionServices(engine: engine, db: db, status: status);
    final dir = c.dataDir ?? paths.context.join(paths.dataDir, 'connections', c.name);
    return ConnectionServices(
      engine: engine,
      db: SqliteStateReader.file(paths.dbPathIn(dir)),
      status: FileStatusSource(paths.statusPathIn(dir)),
    );
  }

  /// How often the local state (status.json, the database) is read.
  final Duration tick;

  /// The CLI (status, settings) is asked every this many ticks.
  final int engineEvery;

  /// How long the search box waits after the last key press.
  final Duration searchDelay;

  /// Defaults to true on Windows.
  final bool? caseInsensitiveFs;

  /// The sync folder is a Windows Cloud Files sync root: an item freed (by `clouddrop free`
  /// or in Explorer) stays on disk as a placeholder that downloads when opened. Defaults to
  /// true on Windows; on Linux a freed file is simply gone.
  final bool? cloudFilesSyncRoot;

  /// Whether something exists at an absolute path (swapped in tests).
  final Future<bool> Function(String absolute) pathExists;

  bool get hasPlaceholders => cloudFilesSyncRoot ?? Platform.isWindows;
}

Future<bool> _pathExists(String path) async =>
    await FileSystemEntity.type(path, followLinks: false) != FileSystemEntityType.notFound;

enum Severity { info, success, warning, error }

/// A short message for an InfoBar toast.
class AppMessage {
  const AppMessage(this.text, [this.severity = Severity.info, this.actionLabel, this.onAction]);
  final String text;
  final Severity severity;
  final String? actionLabel;
  final VoidCallback? onAction;
}

/// The four screens.
enum AppPane { home, files, activity, settings }

/// A request to open the Files screen at some place.
class FilesNav {
  const FilesNav({this.dir = '', this.filter = Filter.all, this.select});
  final String dir;
  final Filter filter;

  /// Entry path to scroll to and select.
  final String? select;
}

/// One connection as the switcher shows it: its status dot and line.
@immutable
class ConnectionEntry {
  const ConnectionEntry(this.connection, this.icon, this.line, {this.selected = false});

  final Connection connection;
  final SummaryIcon icon;

  /// "Up to date", "Uploading 3 files — 40%", … (the status chip's words).
  final String line;
  final bool selected;

  String get name => connection.name;
  String get label => connection.label;

  @override
  bool operator ==(Object other) =>
      other is ConnectionEntry &&
      other.connection == connection &&
      other.icon == icon &&
      other.line == line &&
      other.selected == selected;

  @override
  int get hashCode => Object.hash(connection, icon, line, selected);
}

class AppController extends ChangeNotifier {
  AppController(this.services);

  final AppServices services;

  /// The CLI for the selected connection (`--connection`).
  EngineApi get engine => _current?.engine ?? services.engine;

  /// The selected connection's database and status.
  StateReader get db => _current?.db ?? services.db;
  StatusSource get statusSource => _current?.status ?? services.status;
  AppPaths get paths => services.paths;

  // ---------- connections ----------

  /// Every connection (`connections --json`), the default one first; empty before the first
  /// setup, and with an engine from before connections.
  List<Connection> connections = const [];

  /// The engine lists connections (an older one has just the one, and takes no --connection).
  bool connectionsKnown = false;

  String _selected = Connection.defaultName;

  /// The name of the connection Home, Files, Activity and Settings show.
  String get connectionName => _selected;

  /// The selected connection, once the engine has listed it.
  Connection? get connection => _find(_selected);

  /// More than one folder uploads: the switcher shows, and names say which folder.
  bool get several => connections.length > 1;

  /// What the selected connection is called ("CloudDrop" for the default one in ~/CloudDrop).
  String get label => connection?.label ?? 'CloudDrop';

  /// What the top of the folder is called on screen: "CloudDrop" with one folder (as always),
  /// the connection's label with several.
  String get rootName => several ? label : 'CloudDrop';

  /// Each connection with its status dot, for the switcher (empty with one connection).
  List<ConnectionEntry> get switcher => _switcher;
  List<ConnectionEntry> _switcher = const [];

  /// Bumped when another connection is selected: the screens start over at its top.
  final ValueNotifier<int> connectionVersion = ValueNotifier(0);

  ConnectionServices? _current;
  final Map<String, ConnectionServices> _opened = {};

  Connection? _find(String name) {
    for (final c in connections) {
      if (c.name == name) return c;
    }
    return null;
  }

  /// [name] as the engine spells it (connection names are unique ignoring case).
  String? _match(String? name) {
    if (name == null) return null;
    for (final c in connections) {
      if (c.name.toLowerCase() == name.toLowerCase()) return c.name;
    }
    return null;
  }

  ConnectionServices _servicesFor(String name) {
    final open = _opened[name];
    if (open != null) return open;
    final c = _find(name) ?? Connection(name: name, label: name);
    return _opened[name] = services.open(c);
  }

  /// Lets go of what belonged to connections that are gone.
  void _forgetGone() {
    final names = {for (final c in connections) c.name};
    final gone = [
      for (final n in _opened.keys)
        if (!names.contains(n) && n != _selected) n,
    ];
    for (final n in gone) {
      final s = _opened.remove(n)!;
      if (!identical(s.db, services.db)) s.db.close();
      _others.remove(n);
      _otherDaemons.remove(n);
    }
  }

  /// Shows connection [name] from now on: its engine, database and status. Doesn't notify: the
  /// next poll publishes once it has read the new one (no half-read screen in between).
  void _switchTo(String name) {
    final old = _current;
    _selected = name;
    _current = _servicesFor(name);
    // Only the folder shown keeps its database open (Windows can't delete an open file, should
    // the other one be removed).
    if (old != null && !identical(old.db, _current!.db)) old.db.release();
    _switching = true;
    config = null;
    engineStatus = null;
    _haveAggregates = false;
    _lastDataVersion = null;
    _metaPaused = false;
    _daemon = null;
    _rate.add(null);
    _counts = const Counts();
    _cloudBytes = 0;
    _cloudOnlyBytes = 0;
    _root = const FolderStats();
    _paused = false;
    _rootId = null;
    _failures = 0;
    connectionVersion.value++;
    dbVersion.value++;
  }

  /// Another connection was selected and its local state isn't read yet.
  bool _switching = false;

  /// Waits for the round in flight, so nothing it read lands on another connection.
  Future<void> _quiet() async {
    while (_polling != null) {
      await _polling;
    }
  }

  /// Shows connection [name] (the switcher) and remembers the choice in app.json.
  Future<void> selectConnection(String name) async {
    final want = _match(name) ?? name;
    if (want == _selected && _current != null) return;
    await _quiet();
    if (_disposed) return;
    _switchTo(want);
    unawaited(services.prefs.setConnection(want));
    await poll(forceEngine: true);
  }

  /// Reads the list of connections, and picks the one to show: the remembered one at first,
  /// else the first; another one when the selected one is gone.
  Future<void> _readConnections() async {
    final list = await services.engine.connections();
    if (list == null) return; // an engine from before connections, or a failed answer
    final first = !connectionsKnown;
    connectionsKnown = true;
    connections = list;
    if (first) {
      final remembered = _match(await services.prefs.connection());
      _switchTo(remembered ?? (list.isEmpty ? Connection.defaultName : list.first.name));
    } else if (_find(_selected) == null && list.isNotEmpty) {
      _switchTo(list.first.name);
    } else if (list.isEmpty && _selected != Connection.defaultName) {
      _switchTo(Connection.defaultName);
    }
    _forgetGone();
  }

  /// The other connections' `status --json`, every engine round (several only).
  final Map<String, EngineStatus> _others = {};

  /// Their `status.json`, every tick.
  final Map<String, DaemonStatus?> _otherDaemons = {};

  Future<void> _readOtherEngines() async {
    if (!several) {
      _others.clear();
      return;
    }
    final names = [
      for (final c in connections)
        if (c.name != _selected) c.name,
    ];
    final results = await Future.wait([
      for (final n in names)
        _servicesFor(n).engine.status().then<EngineStatus?>((s) => s, onError: (_) => null),
    ]);
    for (var i = 0; i < names.length; i++) {
      final r = results[i];
      if (r != null) _others[names[i]] = r;
    }
  }

  Future<void> _readOtherStatus() async {
    if (!several) {
      _otherDaemons.clear();
      return;
    }
    for (final c in connections) {
      if (c.name == _selected) continue;
      _otherDaemons[c.name] = await _servicesFor(c.name).status.read();
    }
  }

  /// The switcher's rows: the selected connection as Home shows it, the others from their
  /// `status --json` and `status.json`, worded the same way.
  List<ConnectionEntry> _buildSwitcher(Summary mine) {
    if (!several) return const [];
    return [
      for (final c in connections)
        if (c.name == _selected)
          ConnectionEntry(c, mine.icon, mine.line, selected: true)
        else
          _stateOf(c),
    ];
  }

  ConnectionEntry _stateOf(Connection c) {
    final st = _others[c.name];
    final id = st?.provider ?? c.provider ?? CloudProvider.google.id;
    final provider = providers.firstWhere(
      (p) => p.id == id,
      orElse: () => CloudProvider(id: id, name: st?.providerName ?? c.providerName ?? id),
    );
    final configured = st?.configured ?? c.configured;
    final sum = summarize(
      Snapshot(
        cfg: configured
            ? AppConfig(
                syncDir: c.folder ?? '',
                driveFolder: c.driveFolder ?? '',
                clientId: '',
                hasClientSecret: false,
              )
            : null,
        // Not asked yet: the dot waits for the engine rather than crying wolf.
        signedIn: st?.signedIn ?? true,
        daemon: _otherDaemons[c.name],
        counts: Counts(failed: st?.files['error'] ?? 0),
        provider: provider,
      ),
    );
    return ConnectionEntry(c, sum.icon, sum.line);
  }

  // ---------- observable state ----------

  Snapshot snapshot = const Snapshot();
  AppConfig? config;
  EngineStatus? engineStatus;
  ServiceStatus? service;

  /// The first read of the engine finished.
  bool loaded = false;

  /// The engine could not be asked at all: the sentence to show.
  String? fatal;

  /// [fatal] is "the CLI isn't there" (as opposed to "it ran and complained").
  bool fatalIsMissingCli = false;

  /// The setup assistant is showing (stays until the user finishes it).
  bool setupActive = false;

  final ValueNotifier<AppPane> pane = ValueNotifier(AppPane.home);
  final ValueNotifier<FilesNav?> filesNav = ValueNotifier(null);

  /// Changes whenever the engine committed to the database.
  final ValueNotifier<int> dbVersion = ValueNotifier(0);

  /// An action (free up space, download) is running.
  final ValueNotifier<int> busyCount = ValueNotifier(0);

  final StreamController<AppMessage> _messages = StreamController.broadcast();
  Stream<AppMessage> get messages => _messages.stream;

  bool get busy => busyCount.value > 0;
  String? get syncDir => config?.syncDir ?? engineStatus?.folder;
  bool get hasBuiltinClient => engineStatus?.hasBuiltinClient ?? false;

  /// Every cloud CloudDrop can upload to (asked once; just Google Drive until then).
  List<CloudProvider> providers = const [CloudProvider.google];
  bool _providersLoaded = false;

  /// The configured cloud: its entry in [providers], or what `status --json` says about it.
  CloudProvider get provider {
    final st = engineStatus;
    final id = st?.provider ?? CloudProvider.google.id;
    for (final p in providers) {
      if (p.id == id) return p;
    }
    if (id == CloudProvider.google.id) return CloudProvider.google;
    return CloudProvider(id: id, name: st?.providerName ?? id);
  }

  /// The settings file exists but can't be read: the sentence from the engine.
  String? get configError => engineStatus?.configError;

  // ---------- polling ----------

  Timer? _timer;
  int _ticks = 0;
  Future<void>? _polling;
  bool _disposed = false;
  int? _lastDataVersion;

  void start() {
    _timer?.cancel();
    _timer = Timer.periodic(services.tick, (_) => poll());
    poll();
  }

  void stop() {
    _timer?.cancel();
    _timer = null;
  }

  /// Polls are held while a connection's files are being deleted.
  int _held = 0;

  /// One round: the CLI when it's due (or [forceEngine]), then the local files.
  Future<void> poll({bool forceEngine = false}) async {
    if (_polling != null || _disposed || _held > 0) return;
    final done = Completer<void>();
    _polling = done.future;
    try {
      if (forceEngine || !loaded || _ticks % services.engineEvery == 0) {
        await _readEngine();
      }
      _ticks++;
      await _readLocal();
    } catch (e, st) {
      debugPrint('poll failed: $e\n$st');
    } finally {
      _polling = null;
      done.complete();
    }
  }

  /// Re-reads everything now (after an action).
  Future<void> refreshNow() => poll(forceEngine: true);

  Future<void> _readEngine() async {
    try {
      await _readConnections();
      final engine = this.engine;
      final wantProviders = !_providersLoaded;
      final results = await Future.wait<Object?>([
        engine.status(),
        engine.configGet(),
        wantProviders ? services.engine.providers() : Future<Object?>.value(),
        _readOtherEngines(),
      ]);
      engineStatus = results[0] as EngineStatus;
      config = results[1] as AppConfig?;
      if (wantProviders) {
        providers = results[2] as List<CloudProvider>;
        _providersLoaded = true;
      }
      fatal = null;
      fatalIsMissingCli = false;
      _failures = 0;
    } on CliUnavailable catch (e) {
      _failed(e.toString(), missingCli: true);
    } on StateError catch (e) {
      _failed(e.message, missingCli: false);
    }
    // The first good answer decides whether the setup assistant is due. A settings file that
    // exists but can't be read is not "not set up": the assistant would overwrite it. With
    // several folders, one that needs a sign-in says so on Home instead.
    final st = engineStatus;
    if (fatal == null && !_gateChecked && st != null && st.configError == null) {
      _gateChecked = true;
      setupActive = !st.ready && !several;
    }
    loaded = true;
    // Just switched: the rest is read before anything is shown.
    if (!_switching) _publish();
  }

  /// One failed answer (a slow or busy moment) keeps what is on screen; a few in a row, or a
  /// failure before there is anything to show, bring up the explanation.
  void _failed(String message, {required bool missingCli}) {
    _failures++;
    if (!loaded || _failures >= 3) {
      fatal = message;
      fatalIsMissingCli = missingCli;
    }
  }

  int _failures = 0;
  bool _gateChecked = false;

  Future<void> _readLocal() async {
    final cfg = config;
    final st = engineStatus;
    if (cfg != null && st != null && st.configured) {
      final db = this.db;
      _daemon = await statusSource.read();
      _rate.add(_daemon);
      // The aggregates scan the whole table: redo them only when the database changed.
      final dv = await db.dataVersion();
      final changed = dv == null || dv != _lastDataVersion || !_haveAggregates;
      if (changed) {
        _counts = Counts.fromRows(await db.counts());
        final bytes = await db.cloudBytes();
        _cloudBytes = bytes.all;
        _cloudOnlyBytes = bytes.cloudOnly;
        _root = await db.folderStats('');
        // The engine's own answer (status --json) when the database can't be read.
        _metaPaused = dv == null ? st.paused : await db.meta('paused') == '1';
        _rootId = st.rootId ?? await db.meta('root_id');
        _haveAggregates = true;
      }
      _paused = _metaPaused || _daemon?.phase == Phase.paused;
      if (dv == null || dv != _lastDataVersion) {
        _lastDataVersion = dv;
        dbVersion.value++;
      }
    }
    await _readOtherStatus();
    _switching = false;
    _publish();
  }

  bool _haveAggregates = false;
  bool _metaPaused = false;
  DaemonStatus? _daemon;
  final RateMeter _rate = RateMeter();
  Counts _counts = const Counts();
  int _cloudBytes = 0;
  int _cloudOnlyBytes = 0;
  FolderStats _root = const FolderStats();
  bool _paused = false;
  String? _rootId;

  bool _published = false;
  String? _lastFatal;
  String? _lastConfigError;
  bool _lastSetup = false;
  bool _lastBuiltin = false;
  String _lastSelected = Connection.defaultName;
  List<Connection> _lastConnections = const [];

  /// Builds the snapshot from the latest reads and tells listeners if anything changed.
  void _publish() {
    final cfg = config;
    final st = engineStatus;
    final next = (cfg != null && st != null && st.configured)
        ? Snapshot(
            cfg: cfg,
            signedIn: st.signedIn,
            daemon: _daemon,
            paused: _paused,
            counts: _counts,
            cloudBytes: _cloudBytes,
            cloudOnlyBytes: _cloudOnlyBytes,
            root: _root,
            rootId: _rootId,
            rate: _rate.bytesPerSecond,
            provider: provider,
          )
        : Snapshot(signedIn: st?.signedIn ?? false, provider: provider);
    final switcher = _buildSwitcher(summarize(next));
    final changed =
        !_published ||
        next != snapshot ||
        fatal != _lastFatal ||
        configError != _lastConfigError ||
        setupActive != _lastSetup ||
        hasBuiltinClient != _lastBuiltin ||
        _selected != _lastSelected ||
        !sameConnections(connections, _lastConnections) ||
        !listEquals(switcher, _switcher);
    snapshot = next;
    _switcher = switcher;
    _published = true;
    _lastFatal = fatal;
    _lastConfigError = configError;
    _lastSetup = setupActive;
    _lastBuiltin = hasBuiltinClient;
    _lastSelected = _selected;
    _lastConnections = connections;
    if (changed) notifyListeners();
  }

  @override
  void notifyListeners() {
    if (!_disposed) super.notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    _messages.close();
    // The databases this controller opened (the default one is the caller's).
    for (final s in _opened.values) {
      if (!identical(s.db, services.db)) s.db.close();
    }
    _opened.clear();
    super.dispose();
  }

  // ---------- messages ----------

  void say(String text, [Severity severity = Severity.info]) {
    if (!_messages.isClosed) _messages.add(AppMessage(text, severity));
  }

  void sayWithAction(String text, String label, VoidCallback onAction) {
    if (!_messages.isClosed) {
      _messages.add(AppMessage(text, Severity.info, label, onAction));
    }
  }

  void _report(ActionResult r) => say(r.message, r.ok ? Severity.success : Severity.error);

  // ---------- navigation ----------

  void showFiles({String dir = '', Filter filter = Filter.all, String? select}) {
    filesNav.value = FilesNav(dir: dir, filter: filter, select: select);
    pane.value = AppPane.files;
  }

  // ---------- setup ----------

  /// Which page the next assistant starts on.
  bool setupAtSignIn = false;

  /// The next assistant moves CloudDrop to another provider (Settings › Switch Provider…).
  bool setupSwitching = false;

  /// The next assistant sets up another folder, a new connection ("Add Another Folder…").
  bool setupAdding = false;

  /// Opens the setup assistant ([atSignIn]: straight on its sign-in page; [switching]: to
  /// choose another provider, with a way back; [adding]: for a new connection, with a way back).
  void beginSetup({bool atSignIn = false, bool switching = false, bool adding = false}) {
    setupAtSignIn = atSignIn;
    setupSwitching = switching;
    setupAdding = adding;
    setupActive = true;
    notifyListeners();
  }

  /// "Add Another Folder…": the assistant for a new connection.
  void beginAddConnection() => beginSetup(adding: true);

  /// Leaves an assistant that changed nothing (or whose changes were taken back).
  void cancelSetup() {
    setupActive = false;
    notifyListeners();
  }

  Future<void> finishSetup() async {
    setupActive = false;
    notifyListeners();
    await refreshNow();
  }

  /// The assistant set up the new connection [name]: show it from now on (and remember it).
  Future<void> finishAdding(String name) async {
    await _quiet();
    final list = await services.engine.connections();
    if (list != null) connections = list;
    final want = _match(name);
    if (want != null) {
      _switchTo(want);
      unawaited(services.prefs.setConnection(want));
    }
    setupActive = false;
    // One publish, with the new folder read.
    await poll(forceEngine: true);
  }

  /// Settings › "Remove This Folder…" (the user confirmed): `connections remove`. The folder and
  /// the cloud keep everything. Shows another connection afterwards, or the setup assistant
  /// when it was the last.
  Future<ActionResult> removeConnection() async {
    final name = _selected;
    final label = this.label;
    // Nothing of ours may hold its database open while the CLI deletes it.
    _held++;
    final ActionResult r;
    try {
      await _quiet();
      db.release();
      r = await _busy(() => services.engine.removeConnection(name));
    } finally {
      _held--;
    }
    if (!r.ok) {
      say("Couldn't remove “$label”: ${r.message}", Severity.error);
      return r;
    }
    await _quiet();
    final list = await services.engine.connections() ?? const <Connection>[];
    connections = [
      for (final c in list)
        if (c.name != name) c,
    ];
    final next = connections.isEmpty ? Connection.defaultName : connections.first.name;
    _switchTo(next);
    _forgetGone();
    if (connections.isNotEmpty) unawaited(services.prefs.setConnection(next));
    say('CloudDrop no longer uploads “$label”. Nothing was deleted.', Severity.success);
    await poll(forceEngine: true);
    if (connections.isEmpty) beginSetup();
    return r;
  }

  // ---------- actions ----------

  Future<T> _busy<T>(Future<T> Function() body) async {
    busyCount.value++;
    try {
      return await body();
    } finally {
      busyCount.value--;
    }
  }

  List<String> _abs(Iterable<String> rels) {
    final root = syncDir;
    if (root == null) return const [];
    return [for (final r in rels) paths.absolute(root, r)];
  }

  Future<void> setPaused(bool paused) async {
    final r = await (paused ? engine.pause() : engine.resume());
    _report(r);
    await refreshNow();
  }

  /// Removes verified local copies under each path ("" = everything).
  Future<ActionResult> free(List<String> rels) => _busy(() async {
    final r = await engine.free(_abs(rels));
    _report(r);
    await refreshNow();
    return r;
  });

  /// Downloads cloud-only files under each path.
  Future<ActionResult> restore(List<String> rels, String what) => _busy(() async {
    say('Downloading $what…');
    final r = await engine.restore(_abs(rels));
    _report(r);
    await refreshNow();
    return r;
  });

  Future<ActionResult> retry(List<String> rels) async {
    final r = await engine.retry(rels.length == 1 && rels.first.isEmpty ? const [] : _abs(rels));
    _report(r);
    await refreshNow();
    return r;
  }

  /// Pins or unpins a folder: "Always keep on this computer".
  Future<ActionResult> setKeep(String rel, bool keep) async {
    final r = await (keep ? engine.keep(_abs([rel])) : engine.unkeep(_abs([rel])));
    _report(r);
    await refreshNow();
    return r;
  }

  /// `config set`, applied live by the engine; [success] replaces the engine's sentence.
  Future<ActionResult> setSetting(String key, String value, {String? success}) async {
    final r = await engine.configSet(key, value);
    if (r.ok) {
      // Show the new value at once; the engine confirms on the next read.
      config = _withSetting(config, key, value);
      say(success ?? 'Settings saved', Severity.success);
      _publish();
      unawaited(poll(forceEngine: true));
    } else {
      say("Couldn't save: ${r.message}", Severity.error);
    }
    return r;
  }

  AppConfig? _withSetting(AppConfig? c, String key, String value) {
    if (c == null) return null;
    switch (key) {
      case 'parallel_uploads':
        return c.copyWith(parallelUploads: int.tryParse(value));
      case 'settle_secs':
        return c.copyWith(settleSecs: int.tryParse(value));
      case 'notifications':
        return c.copyWith(notifications: value == 'true' || value == 'on' || value == '1');
      case 'ignore':
        return c.copyWith(
          ignore: [
            for (final p in value.split(','))
              if (p.trim().isNotEmpty) p.trim(),
          ],
        );
      case 'auto_free_days':
        return c.copyWith(
          autoFreeDays: value == 'never' ? null : (value == 'now' ? 0 : int.tryParse(value)),
        );
      case 'keep_local':
        return c.copyWith(
          keepLocal: [
            for (final p in value.split(','))
              if (p.trim().isNotEmpty) p.trim(),
          ],
        );
    }
    return c;
  }

  /// `clouddrop logout`, then back to the sign-in page of the setup assistant. With several
  /// folders Home offers the sign-in instead, so the others stay a click away.
  Future<ActionResult> signOut() async {
    final r = await engine.logout();
    _report(r);
    if (r.ok) {
      await refreshNow();
      if (!several) beginSetup(atSignIn: true);
    }
    return r;
  }

  Future<void> refreshService() async {
    service = await services.engine.serviceStatus();
    notifyListeners();
  }

  Future<void> setStartAtSignIn(bool on) async {
    final r = await (on ? services.engine.serviceEnable() : services.engine.serviceDisable());
    _report(r);
    await refreshService();
    await refreshNow();
  }

  Future<void> startService() async {
    final r = await services.engine.serviceStart();
    _report(r);
    await refreshNow();
  }

  // ---------- opening things ----------

  /// Windows: a freed item that is still on disk as a placeholder, which Explorer (and so
  /// `open`) downloads on demand. Always false where freed files are deleted (Linux).
  Future<bool> isPlaceholder(String rel) async {
    final root = syncDir;
    if (root == null || !services.hasPlaceholders) return false;
    return services.pathExists(paths.absolute(root, rel));
  }

  /// The selected connection's settings file (`config.toml`).
  String get configPath => connection?.config ?? paths.configPath;

  /// Opens the folder that holds its `config.toml` (to fix or move the file by hand).
  void openSettingsFolder() => services.system.openPath(paths.context.dirname(configPath));

  void openFolder() {
    final root = syncDir;
    if (root != null) services.system.openPath(root);
  }

  /// Opens a file with its default app, or a folder in Explorer.
  void openLocal(String rel) {
    final root = syncDir;
    if (root != null) services.system.openPath(paths.absolute(root, rel));
  }

  /// Reveals an item in Explorer (its nearest existing folder if the item is cloud-only).
  void showInExplorer(String rel) {
    final root = syncDir;
    if (root == null) return;
    var abs = paths.absolute(root, rel);
    var r = rel;
    while (!_exists(abs) && r.isNotEmpty) {
      r = parentOf(r);
      abs = paths.absolute(root, r);
    }
    services.system.reveal(abs);
  }

  /// Opens an item's web page in the cloud: built from its Drive id for Google Drive, asked of
  /// the engine (`clouddrop url`) for the others.
  Future<void> openInDrive(Entry e) async {
    final id = e.driveId;
    if (id == null) return;
    if (provider.isGoogle) {
      await services.system.openUrl(driveUrl(e.kind, id));
      return;
    }
    final root = syncDir;
    if (root == null) return;
    final r = await engine.driveUrl(paths.absolute(root, e.path));
    if (r.ok && r.message.isNotEmpty) {
      await services.system.openUrl(r.message);
    } else {
      say(r.message, Severity.error);
    }
  }

  /// The top CloudDrop folder in the cloud: the engine's `clouddrop url` link, or (Google
  /// Drive) the folder id recorded in the database if the CLI can't say.
  Future<void> openRootInDrive() async {
    final root = syncDir;
    if (root != null) {
      final r = await engine.driveUrl(root);
      if (r.ok && r.message.isNotEmpty) {
        await services.system.openUrl(r.message);
        return;
      }
      final id = snapshot.rootId;
      if (id == null || !provider.isGoogle) {
        say(r.message, Severity.error);
        return;
      }
    }
    final id = snapshot.rootId;
    if (id != null && provider.isGoogle) {
      await services.system.openUrl(driveUrl(EntryKind.dir, id));
    }
  }

  bool _exists(String path) =>
      FileSystemEntity.typeSync(path, followLinks: false) != FileSystemEntityType.notFound;

  // ---------- adding files ----------

  /// Lets the user pick files and copies them into [destRel] ("" = the top folder).
  Future<void> addFiles(String destRel) async {
    final picked = await services.system.pickFiles(title: 'Add Files to $rootName');
    if (picked.isEmpty) return;
    await copyIn(destRel, picked);
  }

  /// Copies files and folders into [destRel] without replacing anything.
  Future<void> copyIn(String destRel, List<String> sources) async {
    final root = syncDir;
    if (root == null || !snapshot.ready) {
      say("CloudDrop isn't ready yet", Severity.warning);
      return;
    }
    if (sources.isEmpty) return;
    final destName = destRel.isEmpty ? rootName : nameOf(destRel);
    final reserved = reservedFor(await db.listDir(destRel));
    final report = await copyIntoAsync(
      paths.absolute(root, destRel),
      sources,
      reservedNames: reserved,
      caseInsensitive:
          services.caseInsensitiveFs ?? defaultTargetPlatform == TargetPlatform.windows,
    );
    say(report.summary(destName), report.problems.isEmpty ? Severity.success : Severity.warning);
    await refreshNow();
  }
}

String driveUrl(EntryKind kind, String id) => kind == EntryKind.dir
    ? 'https://drive.google.com/drive/folders/$id'
    : 'https://drive.google.com/file/d/$id/view';
