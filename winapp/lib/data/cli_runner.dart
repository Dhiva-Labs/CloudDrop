/// Running the `clouddrop` CLI (always off the UI thread: `Process` is asynchronous).
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// The outcome of one CLI call.
class CliResult {
  const CliResult(this.exitCode, this.stdout, this.stderr);

  final int exitCode;
  final String stdout;
  final String stderr;

  bool get ok => exitCode == 0;

  /// The sentence to show: stdout on success, the engine's error on failure.
  String get message => ok ? stdout.trim() : errorMessage;

  /// The error sentence from stderr: the engine prints `clouddrop: <reason>` last;
  /// log lines before it are dropped.
  String get errorMessage {
    final lines = stderr
        .split(RegExp(r'\r?\n'))
        .map((l) => l.trim())
        .where((l) => l.isNotEmpty)
        .toList();
    for (final l in lines.reversed) {
      if (l.startsWith('clouddrop: ')) return l.substring('clouddrop: '.length);
    }
    if (lines.isNotEmpty) return lines.last;
    final out = stdout.trim();
    return out.isNotEmpty ? out : 'clouddrop exited with code $exitCode';
  }
}

/// The CLI could not be started at all (not found, not executable).
class CliUnavailable implements Exception {
  CliUnavailable(this.binary, this.cause);
  final String binary;
  final Object cause;

  @override
  String toString() => "Couldn't run $binary: $cause";
}

/// A running CLI process whose output is read while it runs (`login --no-browser`).
abstract class CliProcess {
  /// stdout, line by line.
  Stream<String> get lines;
  Future<int> get exitCode;

  /// Everything the process wrote to stderr (complete once [exitCode] completed).
  Future<String> get stderrText;
  void kill();
}

abstract class CliRunner {
  Future<CliResult> run(List<String> args, {Map<String, String>? environment, Duration? timeout});

  Future<CliProcess> start(List<String> args);
}

/// Runs the real `clouddrop` binary.
class ProcessCliRunner implements CliRunner {
  ProcessCliRunner(this.binary, {this.environment, this.workingDirectory});

  final String binary;

  /// Added to (and overriding) the inherited environment.
  final Map<String, String>? environment;
  final String? workingDirectory;

  Map<String, String>? _env(Map<String, String>? extra) {
    if (environment == null && extra == null) return null;
    return {...?environment, ...?extra};
  }

  @override
  Future<CliResult> run(
    List<String> args, {
    Map<String, String>? environment,
    Duration? timeout,
  }) async {
    final Process proc;
    try {
      proc = await Process.start(
        binary,
        args,
        environment: _env(environment),
        workingDirectory: workingDirectory,
      );
    } on ProcessException catch (e) {
      throw CliUnavailable(binary, e.message);
    }
    // The engine speaks UTF-8; the system code page on Windows is not.
    final out = proc.stdout.transform(utf8.decoder).join();
    final err = proc.stderr.transform(utf8.decoder).join();
    final int code;
    if (timeout == null) {
      code = await proc.exitCode;
    } else {
      try {
        code = await proc.exitCode.timeout(timeout);
      } on TimeoutException {
        proc.kill();
        return const CliResult(124, '', 'clouddrop: timed out');
      }
    }
    return CliResult(code, await out, await err);
  }

  @override
  Future<CliProcess> start(List<String> args) async {
    try {
      final proc = await Process.start(
        binary,
        args,
        environment: _env(null),
        workingDirectory: workingDirectory,
      );
      return _IoCliProcess(proc);
    } on ProcessException catch (e) {
      throw CliUnavailable(binary, e.message);
    }
  }
}

class _IoCliProcess implements CliProcess {
  _IoCliProcess(this._proc) {
    _stderr = _proc.stderr.transform(utf8.decoder).join();
  }

  final Process _proc;
  late final Future<String> _stderr;

  @override
  Stream<String> get lines => _proc.stdout.transform(utf8.decoder).transform(const LineSplitter());

  @override
  Future<int> get exitCode => _proc.exitCode;

  @override
  Future<String> get stderrText => _stderr;

  @override
  void kill() {
    _proc.kill();
  }
}
