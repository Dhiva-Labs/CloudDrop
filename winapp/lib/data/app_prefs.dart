/// `app.json` in the data directory: what the apps remember between runs, shared by the GTK
/// app and this one (`{"connection": name}`, the connection shown last). It belongs to the
/// whole installation, so removing a connection (even the default one) keeps it.
library;

import 'dart:convert';
import 'dart:io';

abstract class AppPrefs {
  /// The connection shown last, if one was remembered.
  Future<String?> connection();

  /// Remembers [name] as the connection to show next time.
  Future<void> setConnection(String name);
}

/// Reads and writes `app.json`, keeping whatever else another app stored in it.
class FileAppPrefs implements AppPrefs {
  FileAppPrefs(this.path);

  final String path;

  Future<Map<String, dynamic>> _read() async {
    try {
      final v = jsonDecode(await File(path).readAsString());
      return v is Map<String, dynamic> ? v : <String, dynamic>{};
    } on FileSystemException {
      return <String, dynamic>{};
    } on FormatException {
      return <String, dynamic>{};
    }
  }

  @override
  Future<String?> connection() async {
    final v = (await _read())['connection'];
    return v is String && v.isNotEmpty ? v : null;
  }

  @override
  Future<void> setConnection(String name) async {
    try {
      final j = await _read();
      if (j['connection'] == name) return;
      j['connection'] = name;
      final file = File(path);
      await file.parent.create(recursive: true);
      // Written beside and renamed over, so a reader never sees half a file.
      final tmp = File('$path.tmp');
      await tmp.writeAsString(const JsonEncoder.withIndent('  ').convert(j));
      await tmp.rename(path);
    } on FileSystemException {
      // Only a convenience: the first connection is shown next time instead.
    }
  }
}

/// Remembers in memory only (tests, and a data directory that can't be written).
class MemoryAppPrefs implements AppPrefs {
  MemoryAppPrefs([this.remembered]);

  String? remembered;

  @override
  Future<String?> connection() async => remembered;

  @override
  Future<void> setConnection(String name) async => remembered = name;
}
