/// Choices and wording of the Settings screen (`gui/src/settings.rs`).
library;

/// The choices offered for automatic space saving: never, right after upload, N days.
const List<int?> autoFreePresets = [null, 0, 7, 30, 90];

String autoFreeLabel(int? days) {
  if (days == null) return 'Never';
  if (days == 0) return 'Right after uploading';
  if (days == 1) return 'After 1 day';
  return 'After $days days';
}

/// The presets, plus the configured value when it is a custom one.
List<int?> autoFreeOptions(int? current) {
  final opts = [...autoFreePresets];
  if (!opts.contains(current)) {
    var at = opts.indexWhere((o) => o != null && current != null && o > current);
    if (at < 0) at = opts.length;
    opts.insert(at, current);
  }
  return opts;
}

/// The value for `clouddrop config set auto_free_days`.
String autoFreeCliValue(int? days) {
  if (days == null) return 'never';
  if (days == 0) return 'now';
  return '$days';
}

String keepSummary(int n) {
  if (n == 0) return 'None';
  if (n == 1) return '1 folder';
  return '$n folders';
}

/// The value for `clouddrop config set ignore|keep_local` (comma-separated).
String commaList(Iterable<String> items) => items.join(', ');

/// Parses what the user typed in the "ignored files" box.
List<String> parseCommaList(String text) => [
  for (final part in text.split(','))
    if (part.trim().isNotEmpty) part.trim(),
];
