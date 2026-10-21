/// Titles and subtitles of the Activity list (`gui/src/activity.rs`).
library;

import '../data/models.dart';
import 'fmt.dart';

/// Which rows the Activity screen lists.
enum ActivityFilter {
  all('All activity'),
  uploads('Uploads'),
  freed('Space freed'),
  downloads('Downloads'),
  problems('Problems');

  const ActivityFilter(this.label);
  final String label;

  bool matches(ActivityKind k) => switch (this) {
    ActivityFilter.all => true,
    ActivityFilter.uploads =>
      k == ActivityKind.uploaded || k == ActivityKind.updated || k == ActivityKind.moved,
    ActivityFilter.freed =>
      k == ActivityKind.freed || k == ActivityKind.autoFreed || k == ActivityKind.keptInCloud,
    ActivityFilter.downloads => k == ActivityKind.restored,
    ActivityFilter.problems => k == ActivityKind.failed,
  };

  /// What the list says when nothing matches.
  String get emptyText => switch (this) {
    ActivityFilter.all => 'Uploads, downloads and other changes will be listed here.',
    ActivityFilter.uploads => 'Nothing has been uploaded recently.',
    ActivityFilter.freed => 'No space has been freed recently.',
    ActivityFilter.downloads => 'Nothing has been downloaded back recently.',
    ActivityFilter.problems => 'No uploads have failed recently.',
  };
}

/// Colour family of an activity icon.
enum Tone { ok, info, warn }

/// Icon choice, mapped to a Fluent icon by the UI.
enum ActivityIcon { uploaded, updated, moved, keptInCloud, freed, restored, failed }

class Described {
  const Described(this.icon, this.tone, this.title, [this.detail]);
  final ActivityIcon icon;
  final Tone tone;
  final String title;

  /// Extra line: where a file moved from, or why it failed.
  final String? detail;
}

/// What a row says; [cloud] names where CloudDrop uploads ("Google Drive").
Described describe(Activity a, {String cloud = 'Google Drive'}) {
  final name = nameOf(a.path);
  switch (a.kind) {
    case ActivityKind.uploaded:
      return Described(ActivityIcon.uploaded, Tone.ok, 'Uploaded $name');
    case ActivityKind.updated:
      return Described(ActivityIcon.updated, Tone.ok, 'Updated $name');
    case ActivityKind.moved:
      final old = a.detail ?? '';
      final sameFolder = parentOf(old) == parentOf(a.path);
      final title = sameFolder ? 'Renamed $name' : 'Moved $name';
      final from = sameFolder ? nameOf(old) : old;
      return Described(ActivityIcon.moved, Tone.info, title, from.isEmpty ? null : 'from $from');
    case ActivityKind.keptInCloud:
      return Described(
        ActivityIcon.keptInCloud,
        Tone.info,
        'Kept $name in $cloud',
        'Removed from this computer',
      );
    case ActivityKind.freed:
      return Described(ActivityIcon.freed, Tone.info, 'Freed up $name');
    case ActivityKind.autoFreed:
      return Described(ActivityIcon.freed, Tone.info, 'Automatically freed up $name');
    case ActivityKind.restored:
      return Described(ActivityIcon.restored, Tone.ok, 'Downloaded $name');
    case ActivityKind.failed:
      return Described(
        ActivityIcon.failed,
        Tone.warn,
        "Couldn't upload $name",
        a.detail?.split('\n').first,
      );
  }
}

/// "Photos · from Trips/a.jpg · 1.2 MB · 12:09"; [root] names the top of the folder.
String subtitle(Activity a, Civil time, {String root = 'CloudDrop'}) {
  final d = describe(a);
  final parent = parentOf(a.path);
  final parts = <String>[parent.isEmpty ? root : parent];
  if (d.detail != null) parts.add(d.detail!);
  final b = a.bytes;
  if (b != null && b > 0 && a.kind != ActivityKind.failed) parts.add(humanBytes(b));
  parts.add(time.clock);
  return parts.join(' · ');
}
