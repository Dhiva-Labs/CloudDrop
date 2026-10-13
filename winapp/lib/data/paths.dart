/// Where the engine keeps its files, exactly as docs/WINDOWS.md describes.
library;

import 'dart:io';

import 'package:path/path.dart' as p;

import 'fs_ops.dart' as fs;

class AppPaths {
  AppPaths({
    required this.configPath,
    required this.dataDir,
    required this.defaultSyncDir,
    required this.cliBinary,
    this.homeDir,
    p.Context? context,
  }) : _p = context ?? p.context;

  /// `config.toml` (`CLOUDDROP_CONFIG` overrides).
  final String configPath;

  /// Directory of `state.db`, `status.json` and `token.json` (`CLOUDDROP_DATA_DIR` overrides).
  final String dataDir;

  /// `%USERPROFILE%\CloudDrop` (`~/CloudDrop` on Linux).
  final String defaultSyncDir;

  /// The `clouddrop` CLI to run.
  final String cliBinary;

  /// The user's profile folder (shown as "~" in paths).
  final String? homeDir;

  final p.Context _p;

  /// The path as shown to the user: on Linux the profile folder is shortened to "~"; on
  /// Windows people see the real path.
  String pretty(String path) {
    if (_p.style == p.Style.windows) return path;
    final home = homeDir;
    if (home != null && home.isNotEmpty && (_p.equals(path, home) || _p.isWithin(home, path))) {
      final rel = _p.relative(path, from: home);
      return rel == '.' ? '~' : '~${_p.separator}$rel';
    }
    return path;
  }

  /// [picked] relative to [syncDir] ('/' separated) if it is a folder beneath it.
  String? relInSync(String syncDir, String picked) => fs.relInSync(syncDir, picked, context: _p);

  String get dbPath => _p.join(dataDir, 'state.db');
  String get statusPath => _p.join(dataDir, 'status.json');
  String get tokenPath => _p.join(dataDir, 'token.json');

  /// Resolves everything from the environment. [env], [windows] and [exeDir] exist for tests.
  factory AppPaths.resolve({Map<String, String>? env, bool? windows, String? exeDir}) {
    final e = env ?? Platform.environment;
    final win = windows ?? Platform.isWindows;
    final ctx = win ? p.windows : p.posix;
    String? get(String k) {
      final v = e[k];
      return v == null || v.isEmpty ? null : v;
    }

    final home = win ? (get('USERPROFILE') ?? get('HOME')) : get('HOME');
    String config;
    String data;
    if (win) {
      final appData = get('APPDATA') ?? ctx.join(home ?? r'C:\', 'AppData', 'Roaming');
      final local = get('LOCALAPPDATA') ?? ctx.join(home ?? r'C:\', 'AppData', 'Local');
      config = ctx.join(appData, 'clouddrop', 'config.toml');
      data = ctx.join(local, 'clouddrop');
    } else {
      final cfgHome = get('XDG_CONFIG_HOME') ?? ctx.join(home ?? '.', '.config');
      final dataHome = get('XDG_DATA_HOME') ?? ctx.join(home ?? '.', '.local', 'share');
      config = ctx.join(cfgHome, 'clouddrop', 'config.toml');
      data = ctx.join(dataHome, 'clouddrop');
    }
    return AppPaths(
      configPath: get('CLOUDDROP_CONFIG') ?? config,
      dataDir: get('CLOUDDROP_DATA_DIR') ?? data,
      defaultSyncDir: ctx.join(home ?? '.', 'CloudDrop'),
      cliBinary: _findCli(e, win, ctx, exeDir),
      homeDir: home,
      context: ctx,
    );
  }

  /// `CLOUDDROP_BIN` wins (an explicit override), then the CLI next to the app,
  /// then `PATH`.
  static String _findCli(Map<String, String> env, bool win, p.Context ctx, String? exeDir) {
    final name = win ? 'clouddrop.exe' : 'clouddrop';
    final override = env['CLOUDDROP_BIN'];
    if (override != null && override.isNotEmpty) return override;
    final dir = exeDir ?? _safeExeDir();
    if (dir != null) {
      final beside = ctx.join(dir, name);
      if (File(beside).existsSync()) return beside;
    }
    final path = env['PATH'] ?? env['Path'] ?? '';
    for (final d in path.split(win ? ';' : ':')) {
      if (d.isEmpty) continue;
      final candidate = ctx.join(d, name);
      if (File(candidate).existsSync()) return candidate;
    }
    return name;
  }

  static String? _safeExeDir() {
    try {
      return p.dirname(Platform.resolvedExecutable);
    } catch (_) {
      return null;
    }
  }

  /// Absolute path of an entry (`rel` uses '/') inside [syncDir].
  String absolute(String syncDir, String rel) {
    if (rel.isEmpty) return syncDir;
    return _p.joinAll([syncDir, ...rel.split('/')]);
  }
}
