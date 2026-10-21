/// Connections: one sync folder uploading to one cloud, each with its own settings, sign-in
/// and state (docs/PROVIDERS.md › "Several clouds at once"). The installation as it always was
/// is the connection `default`; `clouddrop connections --json` lists them all.
library;

import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

/// One entry of `clouddrop connections --json`.
@immutable
class Connection {
  const Connection({
    required this.name,
    required this.label,
    this.folder,
    this.provider,
    this.providerName,
    this.driveFolder,
    this.config,
    this.dataDir,
    this.configured = false,
    this.error,
  });

  /// The name of the connection that was the whole installation before there were several.
  static const String defaultName = 'default';

  /// What `--connection` takes.
  final String name;

  /// What the app shows: the name, or for the default connection its folder's name.
  final String label;

  /// The sync folder; null when the settings can't be read.
  final String? folder;

  /// The cloud's id (`gdrive`, `onedrive`, …) and display name.
  final String? provider;
  final String? providerName;

  /// The top folder in the cloud.
  final String? driveFolder;

  /// Its config.toml.
  final String? config;

  /// Where its state.db and status.json are.
  final String? dataDir;

  /// Set up (Google Drive: with a client).
  final bool configured;

  /// Why its settings can't be read, else null.
  final String? error;

  bool get isDefault => name == defaultName;

  factory Connection.fromJson(Map<String, dynamic> j) {
    String? str(String k) {
      final v = j[k];
      return v is String && v.isNotEmpty ? v : null;
    }

    final name = str('name') ?? defaultName;
    return Connection(
      name: name,
      label: str('label') ?? name,
      folder: str('folder'),
      provider: str('provider'),
      providerName: str('provider_name'),
      driveFolder: str('drive_folder'),
      config: str('config'),
      dataDir: str('data_dir'),
      configured: j['configured'] == true,
      error: str('error'),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is Connection &&
      other.name == name &&
      other.label == label &&
      other.folder == folder &&
      other.provider == provider &&
      other.providerName == providerName &&
      other.driveFolder == driveFolder &&
      other.config == config &&
      other.dataDir == dataDir &&
      other.configured == configured &&
      other.error == error;

  @override
  int get hashCode => Object.hash(
    name,
    label,
    folder,
    provider,
    providerName,
    driveFolder,
    config,
    dataDir,
    configured,
    error,
  );

  @override
  String toString() => 'Connection($name, $label)';
}

/// Parses `clouddrop connections --json`. Throws [FormatException] for anything else.
List<Connection> parseConnections(String json) {
  final Object? v = jsonDecode(json);
  if (v is! List) throw const FormatException('not a list of connections');
  return [
    for (final e in v)
      if (e is Map<String, dynamic>) Connection.fromJson(e),
  ];
}

/// Two lists hold the same connections, in the same order.
bool sameConnections(List<Connection> a, List<Connection> b) => listEquals(a, b);

const int maxNameLength = 32;

final RegExp _nameChar = RegExp(r'[\p{Alphabetic}\p{N} _.\-]', unicode: true);
final RegExp _alnum = RegExp(r'[\p{Alphabetic}\p{N}]', unicode: true);

/// Why [name] can't name a new connection, or null when it can (`connections::check_name`):
/// 1 to 32 letters, digits, spaces, '-', '_' or '.', starting with a letter or digit and not
/// ending in a space or dot; not `default`, not a Windows device name.
String? connectionNameProblem(String name) {
  final runes = name.runes.map(String.fromCharCode).toList();
  if (runes.isEmpty ||
      runes.length > maxNameLength ||
      !_alnum.hasMatch(runes.first) ||
      name.endsWith(' ') ||
      name.endsWith('.') ||
      !runes.every(_nameChar.hasMatch)) {
    return 'Use up to $maxNameLength letters, digits, spaces, "-", "_" or ".", '
        'starting with a letter or digit';
  }
  if (_reserved(name)) return '"$name" is a name Windows reserves';
  if (name.toLowerCase() == Connection.defaultName) return '"default" is taken';
  return null;
}

bool _reserved(String name) {
  final stem = name.split('.').first.trimRight().toUpperCase();
  if (const {'CON', 'PRN', 'AUX', 'NUL'}.contains(stem)) return true;
  return stem.length == 4 &&
      (stem.startsWith('COM') || stem.startsWith('LPT')) &&
      RegExp(r'[0-9]').hasMatch(stem[3]);
}

/// The name of a new connection for [folder], as the engine suggests it
/// (`connections::suggest_name`): the folder's name with characters a name can't hold turned
/// into '-', numbered "Work 2", "Work 3"… when [taken] has it (ignoring case). [taken] may also
/// hold the labels shown, so no two rows of the switcher read the same.
String connectionNameFor(String folder, Iterable<String> taken, {p.Context? context}) {
  final ctx = context ?? p.context;
  final chars = ctx
      .basename(_trimSeparators(folder, ctx))
      .runes
      .map(String.fromCharCode)
      .map((c) => _nameChar.hasMatch(c) ? c : '-')
      .skipWhile((c) => !_alnum.hasMatch(c))
      .take(maxNameLength - 3);
  var base = _tidyEnd(chars.join());
  if (connectionNameProblem(base) != null) {
    base = base.isEmpty ? 'Folder' : '$base folder';
    if (connectionNameProblem(base) != null) base = 'Folder';
  }
  final used = {for (final t in taken) t.toLowerCase()};
  if (!used.contains(base.toLowerCase())) return base;
  for (var k = 2; ; k++) {
    final candidate = '$base $k';
    if (!used.contains(candidate.toLowerCase())) return candidate;
  }
}

String _trimSeparators(String path, p.Context ctx) {
  var s = path;
  while (s.length > 1 && (s.endsWith('/') || (ctx.style == p.Style.windows && s.endsWith(r'\')))) {
    s = s.substring(0, s.length - 1);
  }
  return s;
}

String _tidyEnd(String s) {
  var t = s;
  while (t.endsWith(' ') || t.endsWith('.')) {
    t = t.substring(0, t.length - 1);
  }
  return t;
}

/// How [folder] collides with another connection's folder: it is that folder, lies inside it,
/// or holds it. Each connection needs a folder of its own (the CLI refuses the others).
enum FolderClash { same, inside, holds }

/// The first connection in [connections] whose folder clashes with [folder], and how.
({Connection connection, FolderClash clash})? folderClash(
  String folder,
  Iterable<Connection> connections, {
  p.Context? context,
}) {
  final ctx = context ?? p.context;
  final windows = ctx.style == p.Style.windows;
  String norm(String s) {
    final n = ctx.normalize(ctx.absolute(s));
    return windows ? n.toLowerCase() : n;
  }

  final mine = norm(folder);
  for (final c in connections) {
    final other = c.folder;
    if (other == null || other.isEmpty) continue;
    final theirs = norm(other);
    if (mine == theirs) return (connection: c, clash: FolderClash.same);
    if (ctx.isWithin(theirs, mine)) return (connection: c, clash: FolderClash.inside);
    if (ctx.isWithin(mine, theirs)) return (connection: c, clash: FolderClash.holds);
  }
  return null;
}
