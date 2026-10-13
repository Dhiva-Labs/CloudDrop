/// The app's shared state: the once-a-second snapshot of the engine, the settings, and
/// every action the screens can run. Plain `ChangeNotifier`; all I/O goes through the
/// interfaces in `lib/data`, so tests inject fakes.
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../data/cli_runner.dart';
import '../data/engine_api.dart';
import '../data/fs_ops.dart';
import '../data/models.dart';
import '../data/paths.dart';
import '../data/state_reader.dart';
import '../data/status_source.dart';
import '../data/system_actions.dart';
import '../logic/snapshot.dart';

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
  });

  final AppPaths paths;
  final EngineApi engine;
  final StateReader db;
  final StatusSource status;
  final SystemActions system;

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

class AppController extends ChangeNotifier {
  AppController(this.services);

  final AppServices services;

  EngineApi get engine => services.engine;
  AppPaths get paths => services.paths;

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

  // ---------- polling ----------

  Timer? _timer;
  int _ticks = 0;
  bool _ticking = false;
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

  /// One round: the CLI when it's due (or [forceEngine]), then the local files.
  Future<void> poll({bool forceEngine = false}) async {
    if (_ticking || _disposed) return;
    _ticking = true;
    try {
      if (forceEngine || !loaded || _ticks % services.engineEvery == 0) {
        await _readEngine();
      }
      _ticks++;
      await _readLocal();
    } catch (e, st) {
      debugPrint('poll failed: $e\n$st');
    } finally {
      _ticking = false;
    }
  }

  /// Re-reads everything now (after an action).
  Future<void> refreshNow() => poll(forceEngine: true);

  Future<void> _readEngine() async {
    try {
      final results = await Future.wait<Object?>([engine.status(), engine.configGet()]);
      engineStatus = results[0] as EngineStatus;
      config = results[1] as AppConfig?;
      fatal = null;
      fatalIsMissingCli = false;
      _failures = 0;
    } on CliUnavailable catch (e) {
      _failed(e.toString(), missingCli: true);
    } on StateError catch (e) {
      _failed(e.message, missingCli: false);
    }
    // The first good answer decides whether the setup assistant is due.
    final st = engineStatus;
    if (fatal == null && !_gateChecked && st != null) {
      _gateChecked = true;
      setupActive = !st.ready;
    }
    loaded = true;
    _publish();
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
      final db = services.db;
      _daemon = await services.status.read();
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
    _publish();
  }

  bool _haveAggregates = false;
  bool _metaPaused = false;
  DaemonStatus? _daemon;
  Counts _counts = const Counts();
  int _cloudBytes = 0;
  int _cloudOnlyBytes = 0;
  FolderStats _root = const FolderStats();
  bool _paused = false;
  String? _rootId;

  bool _published = false;
  String? _lastFatal;
  bool _lastSetup = false;
  bool _lastBuiltin = false;

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
          )
        : Snapshot(signedIn: st?.signedIn ?? false);
    final changed =
        !_published ||
        next != snapshot ||
        fatal != _lastFatal ||
        setupActive != _lastSetup ||
        hasBuiltinClient != _lastBuiltin;
    snapshot = next;
    _published = true;
    _lastFatal = fatal;
    _lastSetup = setupActive;
    _lastBuiltin = hasBuiltinClient;
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

  /// Opens the setup assistant ([atSignIn]: straight on its sign-in page).
  void beginSetup({bool atSignIn = false}) {
    setupAtSignIn = atSignIn;
    setupActive = true;
    notifyListeners();
  }

  Future<void> finishSetup() async {
    setupActive = false;
    notifyListeners();
    await refreshNow();
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

  /// `clouddrop logout`, then back to the sign-in page of the setup assistant.
  Future<ActionResult> signOut() async {
    final r = await engine.logout();
    _report(r);
    if (r.ok) {
      await refreshNow();
      beginSetup(atSignIn: true);
    }
    return r;
  }

  Future<void> refreshService() async {
    service = await engine.serviceStatus();
    notifyListeners();
  }

  Future<void> setStartAtSignIn(bool on) async {
    final r = await (on ? engine.serviceEnable() : engine.serviceDisable());
    _report(r);
    await refreshService();
    await refreshNow();
  }

  Future<void> startService() async {
    final r = await engine.serviceStart();
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

  Future<void> openInDrive(Entry e) async {
    final id = e.driveId;
    if (id == null) return;
    await services.system.openUrl(driveUrl(e.kind, id));
  }

  /// The top CloudDrop folder in My Drive: the engine's `clouddrop url` link, or the folder
  /// id recorded in the database if the CLI can't say.
  Future<void> openRootInDrive() async {
    final root = syncDir;
    if (root != null) {
      final r = await engine.driveUrl(root);
      if (r.ok && r.message.isNotEmpty) {
        await services.system.openUrl(r.message);
        return;
      }
      final id = snapshot.rootId;
      if (id == null) {
        say(r.message, Severity.error);
        return;
      }
    }
    final id = snapshot.rootId;
    if (id != null) await services.system.openUrl(driveUrl(EntryKind.dir, id));
  }

  bool _exists(String path) =>
      FileSystemEntity.typeSync(path, followLinks: false) != FileSystemEntityType.notFound;

  // ---------- adding files ----------

  /// Lets the user pick files and copies them into [destRel] ("" = the top folder).
  Future<void> addFiles(String destRel) async {
    final picked = await services.system.pickFiles(title: 'Add Files to CloudDrop');
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
    final destName = destRel.isEmpty ? 'CloudDrop' : nameOf(destRel);
    final reserved = reservedFor(await services.db.listDir(destRel));
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
