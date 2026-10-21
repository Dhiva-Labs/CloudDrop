/// Upload speed, measured from the engine's status heartbeats (`gui/src/snapshot.rs`).
library;

import '../data/models.dart';

/// Bytes per second over the last few seconds of uploading.
///
/// Time comes from the heartbeat's own `updated_ms`, so the reading doesn't depend on when
/// the app happened to look. Bytes are the growth of each upload's `sent` between two
/// heartbeats; an upload that finished between them counts only up to its last report.
class RateMeter {
  RateMeter({this.windowMs = 8000, this.minSpanMs = 2000});

  /// How far back the average reaches.
  final int windowMs;

  /// No reading until the samples span at least this long.
  final int minSpanMs;

  final List<(int ms, int bytes)> _samples = [];
  Map<String, int> _lastSent = const {};
  int? _lastMs;
  int _total = 0;

  /// Feeds one status read. A moment between files (waiting, no upload running) counts as
  /// time without bytes; anything else (idle, paused, offline...) starts the reading over.
  void add(DaemonStatus? d) {
    if (d == null || (d.phase != Phase.uploading && d.phase != Phase.waiting)) {
      _samples.clear();
      _lastSent = const {};
      _lastMs = null;
      return;
    }
    final last = _lastMs;
    if (last != null && d.updatedMs <= last) return;
    final sent = {for (final u in d.uploads) u.path: u.sent};
    for (final e in sent.entries) {
      final before = _lastSent[e.key];
      if (before != null && e.value > before) _total += e.value - before;
    }
    _lastSent = sent;
    _lastMs = d.updatedMs;
    _samples.add((d.updatedMs, _total));
    _samples.removeWhere((s) => s.$1 < d.updatedMs - windowMs);
  }

  /// null until there is enough to measure.
  double? get bytesPerSecond {
    if (_samples.length < 2) return null;
    final (t0, b0) = _samples.first;
    final (t1, b1) = _samples.last;
    if (t1 - t0 < minSpanMs) return null;
    return (b1 - b0) * 1000 / (t1 - t0);
  }
}
