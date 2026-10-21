/// The first-run assistant: Welcome -> where to upload -> Google key (only Google without a
/// built-in client) -> Folder -> Sign in (or a form's Connect) -> Done (`gui/src/setup.rs`).
/// The same assistant switches a folder to another cloud, and adds another folder (a new
/// connection, named after its folder).
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../data/connections.dart';
import '../data/engine_api.dart';
import '../data/providers.dart';
import '../data/system_actions.dart';
import 'app_controller.dart';

/// The six guided steps to create a Google client, with the pages to open.
const List<({String title, String text, String url})> googleKeySteps = [
  (
    title: '1. Create a project',
    text: 'Any name works, for example "CloudDrop".',
    url: 'https://console.cloud.google.com/projectcreate',
  ),
  (
    title: '2. Turn on the Google Drive API',
    text: 'Make sure your new project is selected, then click Enable.',
    url: 'https://console.cloud.google.com/apis/library/drive.googleapis.com',
  ),
  (
    title: '3. Set up the consent screen',
    text: 'Click Get started, enter an app name and your email, choose External.',
    url: 'https://console.cloud.google.com/auth/overview',
  ),
  (
    title: "4. Allow access to CloudDrop's own files",
    text: 'Add or remove scopes → add …/auth/drive.file → Save.',
    url: 'https://console.cloud.google.com/auth/scopes',
  ),
  (
    title: '5. Publish the app',
    text: "Click Publish app, so your sign-in doesn't expire after 7 days.",
    url: 'https://console.cloud.google.com/auth/audience',
  ),
  (
    title: '6. Create a Desktop app client',
    text: 'Application type: Desktop app → Create → Download JSON.',
    url: 'https://console.cloud.google.com/auth/clients/create',
  ),
];

/// `control::looks_like_client_id`.
bool looksLikeClientId(String id) {
  const suffix = '.apps.googleusercontent.com';
  final t = id.trim();
  return t.endsWith(suffix) && t.length > suffix.length;
}

class ClientKey {
  const ClientKey(this.id, this.secret);
  final String id;
  final String secret;
}

/// `control::parse_client_json`: the id/secret from the JSON file Google Cloud offers.
/// Throws [FormatException] with a sentence for the user.
ClientKey parseClientJson(String text) {
  final Object? v;
  try {
    v = jsonDecode(text.trim());
  } on FormatException {
    throw const FormatException('This is not a Google client JSON file');
  }
  if (v is! Map) throw const FormatException('This is not a Google client JSON file');
  Object? inner = v['installed'] ?? v['web'] ?? v;
  if (inner is! Map) inner = v;
  final id = '${inner['client_id'] ?? ''}'.trim();
  final secret = '${inner['client_secret'] ?? ''}'.trim();
  if (id.isEmpty || secret.isEmpty) {
    throw const FormatException(
      'The file has no client_id/client_secret — download the JSON of a "Desktop app" client',
    );
  }
  return ClientKey(id, secret);
}

enum SignInPhase { idle, starting, waiting, done, failed }

/// A browser sign-in: opens the link, waits for the engine to finish.
class SignInFlow extends ChangeNotifier {
  SignInFlow(this.engine, this.system);

  /// Signs in to this engine's connection (the assistant sets a new connection's once it
  /// exists).
  EngineApi engine;
  final SystemActions system;

  SignInPhase phase = SignInPhase.idle;
  String? url;
  String? error;
  LoginSession? _session;
  bool _disposed = false;

  bool get active => phase == SignInPhase.starting || phase == SignInPhase.waiting;

  /// Starts `login --no-browser`, opens the link, and waits. true = signed in.
  Future<bool> start() async {
    cancel();
    phase = SignInPhase.starting;
    error = null;
    url = null;
    _notify();
    final session = engine.login();
    _session = session;
    try {
      final link = await session.url;
      if (!identical(_session, session)) return false;
      url = link;
      phase = SignInPhase.waiting;
      _notify();
      unawaited(system.openUrl(link).catchError((_) {}));
    } catch (e) {
      if (!identical(_session, session)) return false;
      return _fail(e is StateError ? e.message : '$e');
    }
    final result = await session.done;
    if (!identical(_session, session)) return false;
    if (result.ok) {
      phase = SignInPhase.done;
      _notify();
      return true;
    }
    return _fail(result.message);
  }

  bool _fail(String message) {
    phase = SignInPhase.failed;
    error = message;
    _notify();
    return false;
  }

  /// Stops waiting for the browser.
  void cancel() {
    final s = _session;
    _session = null;
    if (s != null && active) {
      s.cancel();
    }
    if (active) {
      phase = SignInPhase.idle;
      _notify();
    }
  }

  /// Opens the link again (the browser may have been closed).
  void reopen() {
    final u = url;
    if (u != null) unawaited(system.openUrl(u).catchError((_) {}));
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    final s = _session;
    _session = null;
    if (s != null && active) s.cancel();
    super.dispose();
  }
}

enum SetupStep { welcome, provider, key, folder, signIn, done }

enum ServiceStart { starting, running, failed }

/// Test Connection on a form provider's page.
enum ConnectionTest { idle, testing, ok, failed }

class SetupController extends ChangeNotifier {
  SetupController(
    this.app, {
    String? lockedRoot,
    bool startAtSignIn = false,
    this.switching = false,
    this.adding = false,
  }) : signIn = SignInFlow(app.engine, app.services.system) {
    // A new folder starts from nothing: no key, folder or settings of the folder shown now.
    final cfg = adding ? null : app.config;
    final existingKey = cfg != null && cfg.hasClient;
    if (existingKey) {
      clientId = cfg.clientId;
      _storedId = cfg.clientId;
      // The secret is never returned by the engine; the key page keeps the stored one.
      hasStoredSecret = true;
    }
    folder = adding ? '' : lockedRoot ?? cfg?.syncDir ?? app.paths.defaultSyncDir;
    folderLocked = !adding && lockedRoot != null;
    driveFolder = cfg?.driveFolder ?? 'CloudDrop';
    final current = app.provider;
    chosen = adding
        ? providers.first
        : providers.firstWhere((p) => p.id == current.id, orElse: () => providers.first);
    _fillSaved();
    signIn.addListener(notifyListeners);
    // After signing out the assistant has nothing to ask but the sign-in.
    if (startAtSignIn) _index = steps.indexOf(SetupStep.signIn);
  }

  final AppController app;
  final SignInFlow signIn;

  /// Moving CloudDrop to another provider (Settings › Switch Provider…): no welcome, a way out
  /// until something is written, and everything uploads again at the end.
  final bool switching;

  /// Adding another folder ("Add Another Folder…"): a new connection named after its folder,
  /// no welcome, and Cancel takes back whatever was written.
  final bool adding;

  /// The new connection, once `connections add` created it (adding only).
  String? createdName;
  String? _createdFolder;

  /// The engine of the connection being set up: the new one when adding.
  EngineApi get _target {
    final name = createdName;
    return name != null ? app.services.engine.forConnection(name) : app.engine;
  }

  /// A Google Drive connection already has a key, which a new one shares.
  bool get _otherGoogleKey =>
      app.connections.any((c) => c.provider == CloudProvider.google.id && c.configured);

  /// The Google key page is due: Google Drive without a built-in client (or, for another
  /// folder, without a key another folder shares).
  bool get _needsKey => chosen.isGoogle && !app.hasBuiltinClient && !(adding && _otherGoogleKey);

  // ---------- provider page ----------

  List<CloudProvider> get providers =>
      app.providers.isEmpty ? const [CloudProvider.google] : app.providers;

  /// Where to upload.
  late CloudProvider chosen;

  /// The provider CloudDrop uploads to now.
  CloudProvider get current => app.provider;

  void choose(CloudProvider p) {
    if (p.id == chosen.id) return;
    chosen = p;
    _fillSaved();
    _clearTest();
    setupError = null;
    notifyListeners();
  }

  // The Google key.
  String clientId = '';
  String clientSecret = '';

  /// The engine already holds a client; the secret is not shown but still in use.
  bool hasStoredSecret = false;
  String? _storedId;

  /// The key file the user chose, while the fields still match it.
  String? keyFile;
  String? keyError;

  // Folders.
  late String folder;
  late bool folderLocked;
  late String driveFolder;

  // Steps.
  int _index = 0;
  List<SetupStep> get steps => [
    if (!switching && !adding) SetupStep.welcome,
    SetupStep.provider,
    if (_needsKey) SetupStep.key,
    SetupStep.folder,
    SetupStep.signIn,
    SetupStep.done,
  ];

  int get index => _index.clamp(0, steps.length - 1);
  SetupStep get step => steps[index];
  int get stepCount => steps.length;

  bool get canGoBack => index > 0 && step != SetupStep.done;

  /// Switching providers can be called off until the new provider's settings are written; adding
  /// a folder until it is done (what was written is removed again).
  bool get canCancel =>
      ((switching && !_switchStarted) ||
          (adding && !settingUp && test != ConnectionTest.testing)) &&
      step != SetupStep.done &&
      !_cancelling;

  bool _cancelling = false;

  /// The header's title.
  String get title => adding ? 'Add Another Folder' : 'Set up CloudDrop';

  /// The header label of the current step.
  String get stepLabel => switch (step) {
    SetupStep.welcome => 'Welcome',
    SetupStep.provider => 'Upload to',
    SetupStep.key => 'Google key',
    SetupStep.folder => 'Folders',
    SetupStep.signIn => chosen.stepLabel,
    SetupStep.done => 'Done',
  };

  void next() {
    if (_index < steps.length - 1) {
      _index++;
      notifyListeners();
      // The last page starts the background engine and reports how that went.
      if (step == SetupStep.done) unawaited(startService());
    }
  }

  void back() {
    if (canGoBack) {
      if (step == SetupStep.signIn) signIn.cancel();
      _index--;
      notifyListeners();
    }
  }

  /// Leaves the assistant without changing anything ([canCancel]); a folder being added is
  /// removed again (`connections remove`) if it was created.
  Future<void> cancel() async {
    if (!canCancel) return;
    signIn.cancel();
    final name = createdName;
    if (name != null) {
      _cancelling = true;
      notifyListeners();
      final r = await app.services.engine.removeConnection(name);
      _cancelling = false;
      if (!r.ok) {
        setupError = r.message;
        notifyListeners();
        return;
      }
      createdName = null;
      app.cancelSetup();
      // The switcher forgets it at once.
      await app.refreshNow();
      return;
    }
    app.cancelSetup();
  }

  // ---------- key page ----------

  bool get keyValid =>
      looksLikeClientId(clientId) && (clientSecret.trim().isNotEmpty || hasStoredSecret);

  void setClientId(String v) {
    // Pasting the whole JSON into the ID field works too.
    if (v.trimLeft().startsWith('{')) {
      _fillFrom(v);
      return;
    }
    clientId = v;
    keyFile = null;
    // A different id needs its own secret; the stored one belongs to the old id.
    if (v.trim() != _storedId) hasStoredSecret = false;
    notifyListeners();
  }

  void setClientSecret(String v) {
    clientSecret = v;
    keyFile = null;
    hasStoredSecret = false;
    notifyListeners();
  }

  Future<void> chooseKeyFile() async {
    final path = await app.services.system.pickJsonFile(initial: app.services.system.downloadsDir);
    if (path == null) return;
    await loadKeyFile(path);
  }

  Future<void> loadKeyFile(String path) async {
    try {
      final text = await _read(path);
      _fillFrom(text, file: path);
    } catch (e) {
      keyError = "Can't read $path";
      notifyListeners();
    }
  }

  /// Overridable for tests.
  Future<String> Function(String path) readFile = (p) => File(p).readAsString();

  Future<String> _read(String path) => readFile(path);

  void _fillFrom(String text, {String? file}) {
    try {
      final k = parseClientJson(text);
      clientId = k.id;
      clientSecret = k.secret;
      hasStoredSecret = false;
      keyFile = file;
      keyError = null;
    } on FormatException catch (e) {
      keyError = e.message;
    }
    notifyListeners();
  }

  // ---------- folder page ----------

  void setDriveFolder(String v) {
    driveFolder = v;
    _driveEdited = true;
    notifyListeners();
  }

  /// The user typed the cloud folder's name (else a new folder's name follows the folder's).
  bool _driveEdited = false;

  bool get folderValid =>
      driveFolder.trim().isNotEmpty && (!adding || (folder.isNotEmpty && folderProblem == null));

  /// Why the chosen folder can't be another folder of CloudDrop's (adding only): it is, holds
  /// or lies inside a folder that already uploads. The engine refuses it too, with its own
  /// sentence.
  String? get folderProblem {
    if (!adding || folder.isEmpty) return null;
    final clash = folderClash(folder, app.connections, context: app.paths.context);
    if (clash == null) return null;
    final other = '“${clash.connection.label}”';
    return switch (clash.clash) {
      FolderClash.same => 'This folder already uploads as $other. Choose another one.',
      FolderClash.inside =>
        'This folder is inside $other, which already uploads. Choose another one.',
      FolderClash.holds => 'This folder holds $other, which already uploads. Choose another one.',
    };
  }

  /// The name the new connection gets: its folder's, made valid and unique ("Work 2").
  String get newConnectionName => connectionNameFor(folder, [
    for (final c in app.connections) ...[c.name, c.label],
  ], context: app.paths.context);

  Future<void> chooseFolder() async {
    if (folderLocked) return;
    final picked = await app.services.system.pickDirectory(
      title: adding ? 'Choose a Folder to Upload' : 'Choose the CloudDrop Folder',
      initial: adding ? (app.paths.homeDir ?? app.paths.defaultSyncDir) : app.paths.defaultSyncDir,
    );
    if (picked != null) {
      folder = picked;
      if (adding && !_driveEdited) {
        final name = app.paths.context.basename(picked);
        if (name.isNotEmpty) driveFolder = name;
      }
      notifyListeners();
    }
  }

  // ---------- sign-in page: Nextcloud's server, a form's fields ----------

  /// The Nextcloud/ownCloud server ([SignInKind.nextcloud]).
  String serverUrl = '';

  bool get serverValid => looksLikeUrl(serverUrl);

  void setServerUrl(String v) {
    serverUrl = v;
    setupError = null;
    notifyListeners();
  }

  /// What the form's fields hold, by key ([SignInKind.form]).
  final Map<String, String> fieldValues = {};

  String fieldValue(String key) => fieldValues[key] ?? '';

  void setField(String key, String v) {
    fieldValues[key] = v;
    _clearTest();
    notifyListeners();
  }

  /// Picks a file for a [FieldKind.file] field (an SSH key).
  Future<void> chooseFieldFile(ProviderField f) async {
    final path = await app.services.system.pickFile(title: f.label);
    if (path != null) setField(f.key, path);
  }

  /// Every field can be saved.
  bool get formValid => chosen.fields.every((f) => f.problem(fieldValue(f.key)) == null);

  ConnectionTest test = ConnectionTest.idle;
  String? testMessage;

  void _clearTest() {
    test = ConnectionTest.idle;
    testMessage = null;
  }

  /// Settings saved for the chosen provider earlier (signed out, or set up again) prefill
  /// the page; secrets never come back.
  void _fillSaved() {
    final saved = adding
        ? const <String, String>{}
        : app.config?.sections[chosen.id] ?? const <String, String>{};
    fieldValues.clear();
    for (final f in chosen.fields) {
      final v = saved[f.key];
      if (v != null && f.kind != FieldKind.secret) fieldValues[f.key] = v;
    }
    serverUrl = saved['url'] ?? '';
  }

  // ---------- sign-in page ----------

  String? setupError;
  bool settingUp = false;
  bool _switchStarted = false;

  /// `--provider` only where the config's provider changes, so a Google setup runs exactly
  /// the command it always did. A new folder always names its provider.
  String? get _providerArg => !adding && chosen.isGoogle && current.isGoogle ? null : chosen.id;

  Future<ActionResult> _runSetup() async {
    if (switching && !_switchStarted) {
      // The old provider's engine must not upload into a database that is about to move on.
      _switchStarted = true;
      await app.services.engine.serviceStop();
    }
    final google = chosen.isGoogle;
    final keepKey = !_needsKey || app.hasBuiltinClient || (hasStoredSecret && keyFile == null);
    final jsonPath = google && _needsKey ? keyFile : null;
    final id = !google || keepKey ? null : clientId.trim();
    final secret = !google || keepKey ? null : clientSecret.trim();
    final settings = switch (chosen.signIn) {
      SignInKind.nextcloud => {'url': serverUrl.trim()},
      SignInKind.form => {
        for (final f in chosen.fields)
          if (f.kind != FieldKind.secret && fieldValue(f.key).trim().isNotEmpty)
            f.key: fieldValue(f.key).trim(),
      },
      SignInKind.google || SignInKind.oauth => const <String, String>{},
    };
    final secrets = {
      if (chosen.signIn == SignInKind.form)
        for (final f in chosen.fields)
          if (f.kind == FieldKind.secret && fieldValue(f.key).isNotEmpty) f.key: fieldValue(f.key),
    };
    if (adding) {
      // Another folder was chosen after the connection was created: it is named after the
      // folder, so it starts over under the new name.
      final made = createdName;
      if (made != null && _createdFolder != folder) {
        signIn.cancel();
        final r = await app.services.engine.removeConnection(made);
        if (!r.ok) return r;
        createdName = null;
      }
      if (createdName == null) {
        final name = newConnectionName;
        final r = await app.services.engine.addConnection(
          name,
          folder: folder,
          driveFolder: driveFolder.trim(),
          clientJsonPath: jsonPath,
          clientId: id,
          clientSecret: secret,
          provider: _providerArg,
          settings: settings,
          secrets: secrets,
        );
        if (r.ok) {
          createdName = name;
          _createdFolder = folder;
          signIn.engine = _target;
        }
        return r;
      }
    }
    // Written before (a sign-in tried again, the form tested again): `setup` updates it.
    return _target.setup(
      folder: folder,
      driveFolder: driveFolder.trim(),
      clientJsonPath: jsonPath,
      clientId: id,
      clientSecret: secret,
      provider: _providerArg,
      settings: settings,
      secrets: secrets,
    );
  }

  /// Writes the config, then signs in in the browser (Google, OAuth, Nextcloud). Moves on
  /// when signed in.
  Future<void> signInWithBrowser() async {
    setupError = null;
    settingUp = true;
    notifyListeners();
    final r = await _runSetup();
    settingUp = false;
    if (!r.ok) {
      setupError = r.message;
      notifyListeners();
      return;
    }
    final ok = await signIn.start();
    if (ok && step == SetupStep.signIn) await _connected();
  }

  /// Already signed in: just write the config and move on.
  Future<void> keepCurrentSignIn() async {
    setupError = null;
    settingUp = true;
    notifyListeners();
    final r = await _runSetup();
    settingUp = false;
    if (!r.ok) {
      setupError = r.message;
      notifyListeners();
      return;
    }
    await _connected();
  }

  /// Signed in to the provider CloudDrop uploads to now (the sign-in there still works).
  bool get alreadySignedIn =>
      (app.engineStatus?.signedIn ?? false) && chosen.id == current.id && !switching && !adding;

  /// Saves the form's settings and secrets, then asks the engine to connect (`check`).
  Future<void> testConnection() async {
    if (!formValid || test == ConnectionTest.testing) return;
    setupError = null;
    test = ConnectionTest.testing;
    testMessage = null;
    notifyListeners();
    var r = await _runSetup();
    if (r.ok) r = await _target.check();
    test = r.ok ? ConnectionTest.ok : ConnectionTest.failed;
    testMessage = r.ok
        ? (r.message.trim().isEmpty ? 'Connected to ${chosen.name}.' : r.message.trim())
        : r.message;
    notifyListeners();
  }

  /// Continue after a successful Test Connection.
  Future<void> continueConnected() async {
    if (test != ConnectionTest.ok) return;
    await _connected();
  }

  /// The new provider works: when switching, everything goes up again; then Done.
  Future<void> _connected() async {
    if (switching) {
      settingUp = true;
      notifyListeners();
      final r = await app.engine.resetCloud();
      settingUp = false;
      if (!r.ok) {
        setupError = r.message;
        notifyListeners();
        return;
      }
    }
    if (step == SetupStep.signIn) next();
  }

  // ---------- done page ----------

  ServiceStart serviceState = ServiceStart.starting;
  String? serviceError;

  Future<void> startService() async {
    serviceState = ServiceStart.starting;
    serviceError = null;
    notifyListeners();
    final engine = app.services.engine;
    if (adding) {
      // The service already uploading the other folders takes this one on by itself; it is
      // only started if it isn't running, leaving "start at sign-in" as the user set it.
      final st = await engine.serviceStatus();
      final r = st.running ? null : await engine.serviceStart();
      serviceState = r == null || r.ok ? ServiceStart.running : ServiceStart.failed;
      serviceError = r == null || r.ok ? null : r.message;
      notifyListeners();
      return;
    }
    final r = await engine.serviceEnable();
    if (r.ok) {
      serviceState = ServiceStart.running;
    } else {
      // It may still be running even if registering for sign-in failed.
      final st = await engine.serviceStatus();
      if (st.running) {
        serviceState = ServiceStart.running;
        serviceError = "It may not start by itself at sign-in: ${r.message}";
      } else {
        serviceState = ServiceStart.failed;
        serviceError = r.message;
      }
    }
    notifyListeners();
  }

  void openFolder() => app.services.system.openPath(folder);

  Future<void> finish() {
    final name = createdName;
    return adding && name != null ? app.finishAdding(name) : app.finishSetup();
  }

  @override
  void dispose() {
    signIn.removeListener(notifyListeners);
    signIn.dispose();
    super.dispose();
  }
}
