/// Human-readable sizes, dates, counts and status labels. Pure functions, unit tested.
/// Wording and rules follow `gui/src/fmt.rs` and `clouddrop::util::human_bytes`.
library;

import '../data/models.dart';

/// `clouddrop::util::human_bytes`: "512 B", "1.5 KB", "3.0 GB".
String humanBytes(int n) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  if (n < 0) n = 0;
  var v = n.toDouble();
  var u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  if (u == 0) return '$n B';
  // One decimal, ties to even like Rust's `{:.1}`.
  final x = v * 10;
  final lo = x.floorToDouble();
  final diff = x - lo;
  final tenths = (diff > 0.5 || (diff == 0.5 && lo % 2 != 0) ? lo + 1 : lo).toInt();
  return '${tenths ~/ 10}.${tenths % 10} ${units[u]}';
}

/// A calendar moment in local time.
class Civil {
  const Civil(this.year, this.month, this.day, this.hour, this.minute);

  final int year;

  /// 1-12
  final int month;
  final int day;
  final int hour;
  final int minute;

  /// Days since 1970-01-01.
  int get days {
    final y = year - (month <= 2 ? 1 : 0);
    final era = _floorDiv(y, 400);
    final yoe = y - era * 400;
    final mp = (month + 9) % 12;
    final doy = (153 * mp + 2) ~/ 5 + day - 1;
    final doe = yoe * 365 + yoe ~/ 4 - yoe ~/ 100 + doy;
    return era * 146097 + doe - 719468;
  }

  /// 0 = Monday.
  int get weekday => (days + 3) % 7;

  String get clock => '${hour.toString().padLeft(2, '0')}:${minute.toString().padLeft(2, '0')}';

  String get monthShort => _months[month - 1].substring(0, 3);

  @override
  bool operator ==(Object other) =>
      other is Civil &&
      other.year == year &&
      other.month == month &&
      other.day == day &&
      other.hour == hour &&
      other.minute == minute;

  @override
  int get hashCode => Object.hash(year, month, day, hour, minute);

  @override
  String toString() => 'Civil($year-$month-$day $clock)';
}

int _floorDiv(int a, int b) => (a / b).floor();

const _months = [
  'January', 'February', 'March', 'April', 'May', 'June', 'July', 'August',
  'September', 'October', 'November', 'December', //
];
const _weekdays = [
  'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday', //
];

/// Tests pin the clock and the time zone so dates on screen (and in goldens) are stable.
DateTime Function() nowProvider = DateTime.now;
bool debugUtcTime = false;

/// Unix milliseconds in the user's local time zone.
Civil civilFromMs(int ms) {
  final d = DateTime.fromMillisecondsSinceEpoch(ms, isUtc: debugUtcTime);
  return Civil(d.year, d.month, d.day, d.hour, d.minute);
}

Civil nowCivil() => civilFromMs(nowProvider().millisecondsSinceEpoch);

/// "Today 12:09", "Yesterday", "12 Oct", "12 Oct 2025".
String shortWhen(Civil then, Civil now) {
  switch (now.days - then.days) {
    case 0:
      return 'Today ${then.clock}';
    case 1:
      return 'Yesterday';
  }
  if (then.year == now.year) return '${then.day} ${then.monthShort}';
  return '${then.day} ${then.monthShort} ${then.year}';
}

/// Section heading for a day in the activity list.
String dayHeading(Civil day, Civil today) {
  final diff = today.days - day.days;
  if (diff == 0) return 'Today';
  if (diff == 1) return 'Yesterday';
  if (diff >= 2 && diff <= 6) return _weekdays[day.weekday];
  final month = _months[day.month - 1];
  if (day.year == today.year) return '${day.day} $month';
  return '${day.day} $month ${day.year}';
}

/// Groups consecutive items that fall on the same day, keeping their order.
List<({String heading, List<T> items})> groupByDay<T>(
  List<T> items,
  int Function(T) at,
  Civil Function(int ms) toCivil,
  Civil today,
) {
  final groups = <({String heading, int key, List<T> items})>[];
  for (final item in items) {
    final c = toCivil(at(item));
    final key = c.days;
    if (groups.isNotEmpty && groups.last.key == key) {
      groups.last.items.add(item);
    } else {
      groups.add((heading: dayHeading(c, today), key: key, items: [item]));
    }
  }
  return [for (final g in groups) (heading: g.heading, items: g.items)];
}

String files(int n) => n == 1 ? '1 file' : '$n files';

String items(int n) => n == 1 ? '1 item' : '$n items';

/// How a file or folder is doing, for the status column.
enum Status {
  uploaded('Uploaded'),
  cloudOnly('Cloud-only'),
  waiting('Waiting'),
  uploading('Uploading'),
  failed('Failed');

  const Status(this.label);
  final String label;

  static Status of(EntryState s) => switch (s) {
    EntryState.synced => Status.uploaded,
    EntryState.cloudOnly => Status.cloudOnly,
    EntryState.pending => Status.waiting,
    EntryState.uploading => Status.uploading,
    EntryState.error => Status.failed,
  };

  /// Order for sorting: problems first.
  int get rank => switch (this) {
    Status.failed => 0,
    Status.uploading => 1,
    Status.waiting => 2,
    Status.cloudOnly => 3,
    Status.uploaded => 4,
  };
}

/// "12 files · 3 cloud-only", "Empty", "5 files · 1 failed".
String folderSummary(FolderStats s) {
  if (s.files == 0) return 'Empty';
  final parts = [files(s.files)];
  if (s.failed > 0) parts.add('${s.failed} failed');
  if (s.waiting > 0) parts.add('${s.waiting} waiting');
  if (s.cloudOnly == s.files) {
    parts.add('all cloud-only');
  } else if (s.cloudOnly > 0) {
    parts.add('${s.cloudOnly} cloud-only');
  }
  return parts.join(' · ');
}

/// The most urgent thing going on inside a folder (null = all uploaded or empty).
Status? folderStatus(FolderStats s) {
  if (s.failed > 0) return Status.failed;
  if (s.waiting > 0) return Status.waiting;
  return null;
}
