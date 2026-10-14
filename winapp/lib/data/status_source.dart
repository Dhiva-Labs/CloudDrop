/// `status.json`: what the engine is doing right now.
library;

import 'dart:convert';
import 'dart:io';

import 'models.dart';

abstract class StatusSource {
  /// The engine's status if it is alive (fresh heartbeat, process exists); else null.
  Future<DaemonStatus?> read();
}

bool defaultPidAlive(int pid) {
  // /proc exists on Linux; elsewhere the 10 s heartbeat is the only liveness signal
  // (a crashed engine stops refreshing the file).
  if (Platform.isLinux) return Directory('/proc/$pid').existsSync();
  return true;
}

/// Reads `status.json` from the engine's data directory.
class FileStatusSource implements StatusSource {
  FileStatusSource(this.path, {int Function()? clock, bool Function(int pid)? pidAlive})
    : _clock = clock ?? (() => DateTime.now().millisecondsSinceEpoch),
      _pidAlive = pidAlive ?? defaultPidAlive;

  final String path;
  final int Function() _clock;
  final bool Function(int pid) _pidAlive;

  @override
  Future<DaemonStatus?> read() async {
    final String text;
    try {
      text = await File(path).readAsString();
    } on FileSystemException {
      return null;
    }
    final Object? json;
    try {
      json = jsonDecode(text);
    } on FormatException {
      return null; // caught mid-write; the next poll gets it
    }
    final s = DaemonStatus.fromJson(json);
    if (s == null || !s.isLive(_clock(), pidAlive: _pidAlive)) return null;
    return s;
  }
}
