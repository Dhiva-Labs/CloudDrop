import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/activity_text.dart';
import 'package:clouddrop_app/logic/fmt.dart';
import 'package:flutter_test/flutter_test.dart';

Activity act(ActivityKind kind, String path, {int? bytes, String? detail}) =>
    Activity(id: 1, atMs: 0, kind: kind, path: path, bytes: bytes, detail: detail);

void main() {
  const noon = Civil(2026, 10, 13, 12, 9);

  test('titles, icons and tones', () {
    final d = describe(act(ActivityKind.uploaded, 'CloudDrop-test/hello.txt', bytes: 72));
    expect(d.title, 'Uploaded hello.txt');
    expect(d.icon, ActivityIcon.uploaded);
    expect(d.tone, Tone.ok);
    expect(describe(act(ActivityKind.freed, 'a/b.mp4')).icon, ActivityIcon.freed);
    expect(describe(act(ActivityKind.restored, 'b.mp4')).title, 'Downloaded b.mp4');
    expect(describe(act(ActivityKind.autoFreed, 'b')).title, 'Automatically freed up b');
    expect(describe(act(ActivityKind.keptInCloud, 'b')).tone, Tone.info);
    expect(describe(act(ActivityKind.keptInCloud, 'b')).title, 'Kept b in Google Drive');
    expect(describe(act(ActivityKind.keptInCloud, 'b')).detail, 'Removed from this computer');
    expect(describe(act(ActivityKind.updated, 'doc.txt')).title, 'Updated doc.txt');
  });

  test('moves and renames', () {
    final m = describe(act(ActivityKind.moved, 'Trips/a.jpg', detail: 'Photos/a.jpg'));
    expect(m.title, 'Moved a.jpg');
    expect(m.detail, 'from Photos/a.jpg');
    final r = describe(act(ActivityKind.moved, 'Photos/new.jpg', detail: 'Photos/old.jpg'));
    expect(r.title, 'Renamed new.jpg');
    expect(r.detail, 'from old.jpg');
  });

  test('failures show the first error line', () {
    final f = describe(
      act(ActivityKind.failed, 'x.bin', bytes: 5, detail: '403 quota exceeded\nmore text'),
    );
    expect(f.title, "Couldn't upload x.bin");
    expect(f.tone, Tone.warn);
    expect(f.detail, '403 quota exceeded');
  });

  test('subtitles', () {
    expect(
      subtitle(act(ActivityKind.uploaded, 'CloudDrop-test/hello.txt', bytes: 72), noon),
      'CloudDrop-test · 72 B · 12:09',
    );
    expect(subtitle(act(ActivityKind.uploaded, 'hello.txt', bytes: 0), noon), 'CloudDrop · 12:09');
    expect(
      subtitle(act(ActivityKind.moved, 'Trips/a.jpg', bytes: 2048, detail: 'Photos/a.jpg'), noon),
      'Trips · from Photos/a.jpg · 2.0 KB · 12:09',
    );
    expect(
      subtitle(act(ActivityKind.failed, 'a/x.bin', bytes: 5, detail: 'boom'), noon),
      'a · boom · 12:09',
    );
  });

  test('filters', () {
    expect(ActivityFilter.uploads.matches(ActivityKind.moved), isTrue);
    expect(ActivityFilter.uploads.matches(ActivityKind.restored), isFalse);
    expect(ActivityFilter.freed.matches(ActivityKind.autoFreed), isTrue);
    expect(ActivityFilter.freed.matches(ActivityKind.keptInCloud), isTrue);
    expect(ActivityFilter.downloads.matches(ActivityKind.restored), isTrue);
    expect(ActivityFilter.problems.matches(ActivityKind.failed), isTrue);
    expect(ActivityFilter.problems.matches(ActivityKind.uploaded), isFalse);
    for (final k in ActivityKind.values) {
      expect(ActivityFilter.all.matches(k), isTrue);
    }
  });
}
