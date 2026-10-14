import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/fmt.dart';
import 'package:flutter_test/flutter_test.dart';

/// Test stand-in for the local time zone: UTC (same helper as gui/src/fmt.rs tests).
Civil utc(int ms) {
  final d = DateTime.fromMillisecondsSinceEpoch(ms, isUtc: true);
  return Civil(d.year, d.month, d.day, d.hour, d.minute);
}

void main() {
  test('humanBytes matches clouddrop::util::human_bytes', () {
    expect(humanBytes(0), '0 B');
    expect(humanBytes(512), '512 B');
    expect(humanBytes(1023), '1023 B');
    expect(humanBytes(1536), '1.5 KB');
    expect(humanBytes(1024), '1.0 KB');
    expect(humanBytes(3 * 1024), '3.0 KB');
    expect(humanBytes(5 * 1024 * 1024), '5.0 MB');
    expect(humanBytes(15 * 1024 * 1024 * 1024), '15.0 GB');
    expect(humanBytes(3 * 1024 * 1024 * 1024 * 1024), '3.0 TB');
    // Larger than TB stays in TB.
    expect(humanBytes(2048 * 1024 * 1024 * 1024 * 1024), '2048.0 TB');
    // Ties round to even, like Rust's {:.1}: 1280 B = 1.25 KB -> "1.2", 1792 B = 1.75 KB -> "1.8".
    expect(humanBytes(1280), '1.2 KB');
    expect(humanBytes(1792), '1.8 KB');
    expect(humanBytes(-5), '0 B');
  });

  test('days and weekdays', () {
    expect(const Civil(1970, 1, 1, 0, 0).days, 0);
    expect(const Civil(1970, 1, 1, 0, 0).weekday, 3); // Thursday
    expect(const Civil(2026, 10, 14, 0, 0).weekday, 2); // Wednesday
    expect(const Civil(2024, 3, 1, 0, 0).days - const Civil(2024, 2, 28, 0, 0).days, 2);
    expect(utc(1791913161130), const Civil(2026, 10, 13, 17, 39));
  });

  test('relative dates', () {
    const now = Civil(2026, 10, 14, 12, 30);
    expect(shortWhen(const Civil(2026, 10, 14, 9, 5), now), 'Today 09:05');
    expect(shortWhen(const Civil(2026, 10, 13, 23, 59), now), 'Yesterday');
    expect(shortWhen(const Civil(2026, 10, 2, 8, 0), now), '2 Oct');
    expect(shortWhen(const Civil(2025, 12, 31, 8, 0), now), '31 Dec 2025');
    // A clock that runs ahead still shows a date, not "Today".
    expect(shortWhen(const Civil(2026, 10, 15, 8, 0), now), '15 Oct');
  });

  test('day headings', () {
    const today = Civil(2026, 10, 14, 12, 0); // Wednesday
    expect(dayHeading(const Civil(2026, 10, 14, 1, 0), today), 'Today');
    expect(dayHeading(const Civil(2026, 10, 13, 1, 0), today), 'Yesterday');
    expect(dayHeading(const Civil(2026, 10, 12, 1, 0), today), 'Monday');
    expect(dayHeading(const Civil(2026, 10, 8, 1, 0), today), 'Thursday');
    expect(dayHeading(const Civil(2026, 10, 7, 1, 0), today), '7 October');
    expect(dayHeading(const Civil(2025, 3, 1, 1, 0), today), '1 March 2025');
  });

  test('grouping keeps order and splits days', () {
    const day = 86400000;
    const base = 1791990000000; // 2026-10-14 ~14:20 UTC
    final today = utc(base);
    final at = [base, base - 3600000, base - day, base - day - 60000, base - 9 * day];
    final groups = groupByDay<int>(at, (t) => t, utc, today);
    expect([for (final g in groups) g.heading], ['Today', 'Yesterday', '5 October']);
    expect(groups[0].items.length, 2);
    expect(groups[1].items.length, 2);
  });

  test('folder labels', () {
    const s = FolderStats(files: 12, cloudOnly: 3);
    expect(folderSummary(s), '12 files · 3 cloud-only');
    expect(folderSummary(const FolderStats()), 'Empty');
    expect(folderSummary(const FolderStats(files: 2, cloudOnly: 2)), '2 files · all cloud-only');
    const bad = FolderStats(files: 5, failed: 1, waiting: 2);
    expect(folderSummary(bad), '5 files · 1 failed · 2 waiting');
    expect(folderStatus(bad), Status.failed);
    expect(folderStatus(s), isNull);
    expect(folderStatus(const FolderStats(files: 1, waiting: 1)), Status.waiting);
    expect(files(1), '1 file');
    expect(files(2), '2 files');
    expect(items(0), '0 items');
    expect(files(1284), '1,284 files');
    expect(items(1000000), '1,000,000 items');
    expect(
      folderSummary(const FolderStats(files: 2410, cloudOnly: 1180, waiting: 1200)),
      '2,410 files · 1,200 waiting · 1,180 cloud-only',
    );
    expect(items(1), '1 item');
  });

  test('status labels and ranking', () {
    expect(Status.of(EntryState.synced).label, 'Uploaded');
    expect(Status.of(EntryState.cloudOnly).label, 'Cloud-only');
    expect(Status.of(EntryState.pending).label, 'Waiting');
    expect(Status.of(EntryState.uploading).label, 'Uploading');
    expect(Status.of(EntryState.error).label, 'Failed');
    expect(Status.failed.rank, lessThan(Status.uploaded.rank));
  });

  test('civilFromMs honours the pinned time zone', () {
    debugUtcTime = true;
    addTearDown(() => debugUtcTime = false);
    expect(civilFromMs(1791913161130), const Civil(2026, 10, 13, 17, 39));
  });
}
