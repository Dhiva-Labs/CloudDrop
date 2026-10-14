/// The first-run assistant: Welcome -> Google key (only without a built-in client) ->
/// Folder -> Sign in -> Done (`gui/src/setup.rs`).
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../data/engine_api.dart';
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

  final EngineApi engine;
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

enum SetupStep { welcome, key, folder, signIn, done }

enum ServiceStart { starting, running, failed }

class SetupController extends ChangeNotifier {
  SetupController(this.app, {String? lockedRoot, bool startAtSignIn = false})
    : signIn = SignInFlow(app.engine, app.services.system) {
    final cfg = app.config;
    final existingKey = cfg != null && cfg.hasClient;
    if (existingKey) {
      clientId = cfg.clientId;
      _storedId = cfg.clientId;
      // The secret is never returned by the engine; the key page keeps the stored one.
      hasStoredSecret = true;
    }
    folder = lockedRoot ?? cfg?.syncDir ?? app.paths.defaultSyncDir;
    folderLocked = lockedRoot != null;
    driveFolder = cfg?.driveFolder ?? 'CloudDrop';
    signIn.addListener(notifyListeners);
    // After signing out the assistant has nothing to ask but the sign-in.
    if (startAtSignIn) _index = steps.indexOf(SetupStep.signIn);
  }

  final AppController app;
  final SignInFlow signIn;

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
    SetupStep.welcome,
    if (!app.hasBuiltinClient) SetupStep.key,
    SetupStep.folder,
    SetupStep.signIn,
    SetupStep.done,
  ];

  int get index => _index.clamp(0, steps.length - 1);
  SetupStep get step => steps[index];
  int get stepCount => steps.length;

  bool get canGoBack => index > 0 && step != SetupStep.done;

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
    notifyListeners();
  }

  bool get folderValid => driveFolder.trim().isNotEmpty;

  Future<void> chooseFolder() async {
    if (folderLocked) return;
    final picked = await app.services.system.pickDirectory(
      title: 'Choose the CloudDrop Folder',
      initial: app.paths.defaultSyncDir,
    );
    if (picked != null) {
      folder = picked;
      notifyListeners();
    }
  }

  // ---------- sign-in page ----------

  String? setupError;
  bool settingUp = false;

  Future<ActionResult> _runSetup() => app.engine.setup(
    folder: folder,
    driveFolder: driveFolder.trim(),
    clientJsonPath: keyFile,
    clientId: app.hasBuiltinClient || (hasStoredSecret && keyFile == null) ? null : clientId.trim(),
    clientSecret: app.hasBuiltinClient || (hasStoredSecret && keyFile == null)
        ? null
        : clientSecret.trim(),
  );

  /// Writes the config, then signs in with Google. Moves on when signed in.
  Future<void> signInWithGoogle() async {
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
    if (ok && step == SetupStep.signIn) next();
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
    next();
  }

  bool get alreadySignedIn => app.engineStatus?.signedIn ?? false;

  // ---------- done page ----------

  ServiceStart serviceState = ServiceStart.starting;
  String? serviceError;

  Future<void> startService() async {
    serviceState = ServiceStart.starting;
    serviceError = null;
    notifyListeners();
    final r = await app.engine.serviceEnable();
    if (r.ok) {
      serviceState = ServiceStart.running;
    } else {
      // It may still be running even if registering for sign-in failed.
      final st = await app.engine.serviceStatus();
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

  Future<void> finish() => app.finishSetup();

  @override
  void dispose() {
    signIn.removeListener(notifyListeners);
    signIn.dispose();
    super.dispose();
  }
}
