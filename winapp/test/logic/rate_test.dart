import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/fmt.dart';
import 'package:clouddrop_app/logic/rate.dart';
import 'package:flutter_test/flutter_test.dart';

DaemonStatus uploading(int ms, Map<String, int> sent) => DaemonStatus(
  pid: 1,
  updatedMs: ms,
  phase: Phase.uploading,
  uploads: [for (final e in sent.entries) Upload(path: e.key, sent: e.value, total: 1 << 30)],
);

void main() {
  test('measures growth of sent bytes over the heartbeats', () {
    final m = RateMeter();
    m.add(uploading(0, {'a': 0}));
    expect(m.bytesPerSecond, isNull);
    m.add(uploading(1000, {'a': 1000}));
    expect(m.bytesPerSecond, isNull, reason: 'less than two seconds measured');
    m.add(uploading(2000, {'a': 2000}));
    expect(m.bytesPerSecond, 1000);
    m.add(uploading(3000, {'a': 3000, 'b': 0}));
    expect(m.bytesPerSecond, 1000);
    m.add(uploading(4000, {'a': 4000, 'b': 1000}));
    expect(m.bytesPerSecond, 1250);
  });

  test('a repeated heartbeat is not a new sample; a finished upload just stops counting', () {
    final m = RateMeter();
    m.add(uploading(0, {'a': 0}));
    m.add(uploading(1000, {'a': 500}));
    m.add(uploading(1000, {'a': 500}));
    m.add(uploading(2000, {'b': 0}));
    expect(m.bytesPerSecond, 250);
  });

  test('a moment between files slows the reading down instead of restarting it', () {
    final m = RateMeter();
    m.add(uploading(0, {'a': 0}));
    m.add(uploading(2000, {'a': 2000}));
    m.add(const DaemonStatus(pid: 1, updatedMs: 4000, phase: Phase.waiting));
    expect(m.bytesPerSecond, 500);
  });

  test('only the last few seconds count', () {
    final m = RateMeter(windowMs: 2000);
    m.add(uploading(0, {'a': 0}));
    m.add(uploading(1000, {'a': 10000}));
    m.add(uploading(2000, {'a': 10100}));
    m.add(uploading(3000, {'a': 10200}));
    expect(m.bytesPerSecond, 100);
  });

  test('anything but uploading starts over', () {
    final m = RateMeter();
    m.add(uploading(0, {'a': 0}));
    m.add(uploading(3000, {'a': 3000}));
    expect(m.bytesPerSecond, 1000);
    m.add(const DaemonStatus(pid: 1, updatedMs: 4000, phase: Phase.paused));
    expect(m.bytesPerSecond, isNull);
    m.add(uploading(5000, {'a': 3000}));
    expect(m.bytesPerSecond, isNull);
  });

  test('speed and time left wording', () {
    expect(speed(2.4 * 1024 * 1024), '2.4 MB/s');
    expect(speed(850 * 1024), '850.0 KB/s');
    expect(timeLeft(20), 'less than a minute left');
    expect(timeLeft(80), 'about a minute left');
    expect(timeLeft(5 * 60 + 10), 'about 5 minutes left');
    expect(timeLeft(3600 + 600), 'about an hour left');
    expect(timeLeft(5 * 3600), 'about 5 hours left');
    expect(timeLeft(30 * 3600), 'more than a day left');
  });
}
