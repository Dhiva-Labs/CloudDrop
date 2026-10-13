import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/state_reader.dart';
import 'package:clouddrop_app/logic/files_model.dart';
import 'package:flutter_test/flutter_test.dart';

DirItem file(
  String path,
  EntryState state,
  int size, {
  String? driveId,
  int? mtimeNs,
  int? uploadedAt,
}) => DirItem.file(
  Entry(
    path: path,
    kind: EntryKind.file,
    state: state,
    cloudSize: size,
    localSize: size,
    driveId: driveId,
    localMtimeNs: mtimeNs,
    uploadedAt: uploadedAt,
  ),
);

DirItem dir(String path, FolderStats stats) => DirItem(
  entry: Entry(path: path, kind: EntryKind.dir, state: EntryState.synced),
  bytes: stats.bytes,
  stats: stats,
);

List<String> names(List<DirItem> v) => [for (final i in v) i.name];

void main() {
  test('filters match files and folders', () {
    final synced = file('a', EntryState.synced, 1);
    final cloud = file('b', EntryState.cloudOnly, 1);
    final wait = file('c', EntryState.pending, 1);
    final up = file('d', EntryState.uploading, 1);
    final bad = file('e', EntryState.error, 1);
    expect(matchesItem(Filter.all, bad), isTrue);
    expect(
      matchesItem(Filter.onComputer, synced) && !matchesItem(Filter.onComputer, cloud),
      isTrue,
    );
    expect(matchesItem(Filter.cloudOnly, cloud) && !matchesItem(Filter.cloudOnly, synced), isTrue);
    expect(matchesItem(Filter.uploading, wait) && matchesItem(Filter.uploading, up), isTrue);
    expect(matchesItem(Filter.failed, bad) && !matchesItem(Filter.failed, wait), isTrue);

    final d = dir('f', const FolderStats(files: 4, local: 3, cloudOnly: 1));
    expect(matchesItem(Filter.onComputer, d), isTrue);
    expect(matchesItem(Filter.cloudOnly, d), isTrue);
    expect(matchesItem(Filter.failed, d), isFalse);
    final empty = dir('g', const FolderStats());
    expect(matchesItem(Filter.all, empty) && !matchesItem(Filter.cloudOnly, empty), isTrue);
  });

  test('filter labels', () {
    expect(
      [for (final f in Filter.values) f.label],
      ['All files', 'On this computer', 'Cloud-only', 'Uploading', 'Failed'],
    );
  });

  test('sorting keeps folders first', () {
    final v = [
      file('b.txt', EntryState.synced, 10),
      dir('Zeta', const FolderStats()),
      file('A.txt', EntryState.cloudOnly, 30),
      dir('alpha', const FolderStats()),
      file('c.txt', EntryState.error, 20),
    ];
    expect(names(sortItems(v, const Sort())), ['alpha', 'Zeta', 'A.txt', 'b.txt', 'c.txt']);
    expect(names(sortItems(v, const Sort(SortKey.name, true))), [
      'Zeta',
      'alpha',
      'c.txt',
      'b.txt',
      'A.txt',
    ]);
    expect(names(sortItems(v, const Sort(SortKey.size, true))).sublist(2), [
      'A.txt',
      'c.txt',
      'b.txt',
    ]);
    expect(names(sortItems(v, const Sort(SortKey.status))).sublist(2), ['c.txt', 'A.txt', 'b.txt']);
  });

  test('dates prefer the file\'s own time', () {
    var e = const Entry(path: 'a', kind: EntryKind.file, state: EntryState.synced);
    expect(itemDateMs(e), isNull);
    e = const Entry(path: 'a', kind: EntryKind.file, state: EntryState.synced, uploadedAt: 5000);
    expect(itemDateMs(e), 5000);
    e = const Entry(
      path: 'a',
      kind: EntryKind.file,
      state: EntryState.synced,
      uploadedAt: 5000,
      localMtimeNs: 2000000000,
    );
    expect(itemDateMs(e), 2000);
    final v = [
      file('new', EntryState.synced, 1, mtimeNs: 9000000000),
      file('none', EntryState.synced, 1),
      file('old', EntryState.synced, 1, mtimeNs: 1000000000),
    ];
    expect(names(sortItems(v, const Sort(SortKey.date))), ['none', 'old', 'new']);
    expect(names(sortItems(v, const Sort(SortKey.date, true))), ['new', 'old', 'none']);
  });

  test('a folder\'s status is its most urgent content, else its own', () {
    expect(itemStatus(dir('d', const FolderStats(files: 2, failed: 1))).label, 'Failed');
    expect(itemStatus(dir('d', const FolderStats(files: 2, waiting: 1))).label, 'Waiting');
    expect(itemStatus(dir('d', const FolderStats(files: 2))).label, 'Uploaded');
  });

  test('capabilities follow the selection', () {
    Keep off(String _) => Keep.off;
    var c = capsFor([file('a', EntryState.synced, 1)], off);
    expect(c.free && !c.download && !c.retry && c.show, isTrue);
    c = capsFor([file('a', EntryState.cloudOnly, 1), file('b', EntryState.error, 1)], off);
    expect(c.download && c.retry && !c.free && !c.openDrive, isTrue);
    expect(capsFor([file('a', EntryState.synced, 1, driveId: 'ID')], off).openDrive, isTrue);

    final d = dir('D', const FolderStats(files: 5, local: 4, cloudOnly: 1, waiting: 1, failed: 1));
    c = capsFor([d], off);
    expect(c.download && c.free && c.retry, isTrue);
    expect(c.keep, Keep.off);
    final onlyPending = dir('E', const FolderStats(files: 2, local: 2, waiting: 2));
    expect(capsFor([onlyPending], off).free, isFalse);
    // The keep toggle is only for a single folder.
    expect(capsFor([d, d], off).keep, Keep.unavailable);
    expect(capsFor([file('a', EntryState.synced, 1)], off).keep, Keep.unavailable);
    expect(capsFor([], off).count, 0);
    expect(capsFor([], off).show, isFalse);
  });

  test('keep state: itself, inherited from a parent, or off', () {
    const list = ['Work', 'Photos/2026/'];
    expect(keepStateFor(list, 'Work'), Keep.on);
    expect(keepStateFor(list, 'Work/Sub'), Keep.inherited);
    expect(keepStateFor(list, 'Photos/2026'), Keep.on);
    expect(keepStateFor(list, 'Photos'), Keep.off);
    expect(keepStateFor(list, 'Workshop'), Keep.off);
  });

  test('empty-state texts per filter', () {
    expect(emptyText(Filter.failed).$1, 'No failed uploads');
    expect(emptyText(Filter.cloudOnly).$1, 'No cloud-only files');
    expect(emptyText(Filter.uploading).$2, 'Everything here has been uploaded.');
    expect(emptyText(Filter.onComputer).$1, 'No files on this computer');
    expect(emptyText(Filter.all).$1, 'Nothing here yet');
  });

  test('matchesState is the one rule for plain files', () {
    expect(matchesState(Filter.onComputer, EntryState.cloudOnly), isFalse);
    expect(matchesState(Filter.onComputer, EntryState.error), isTrue);
  });
}
