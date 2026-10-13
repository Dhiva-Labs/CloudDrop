/// What the Files screen shows: sorting, what a selection allows, empty-state texts
/// (`gui/src/model.rs`).
library;

import '../data/models.dart';
import 'fmt.dart';

enum SortKey { name, status, size, date }

class Sort {
  const Sort([this.key = SortKey.name, this.descending = false]);
  final SortKey key;
  final bool descending;

  @override
  bool operator ==(Object other) =>
      other is Sort && other.key == key && other.descending == descending;

  @override
  int get hashCode => Object.hash(key, descending);
}

Status itemStatus(DirItem it) {
  final s = it.stats;
  if (it.isDir && s != null) {
    return folderStatus(s) ?? Status.of(it.entry.state);
  }
  return Status.of(it.entry.state);
}

/// Modified time in unix ms: the file's own, else when it was uploaded.
int? itemDateMs(Entry e) {
  final ns = e.localMtimeNs;
  if (ns != null && ns > 0) return ns ~/ 1000000;
  return e.uploadedAt;
}

/// Folders first, then by the chosen column (name as tie-break).
List<DirItem> sortItems(List<DirItem> input, Sort sort) {
  final items = [...input];
  items.sort((a, b) {
    final dirs = (a.isDir ? 0 : 1).compareTo(b.isDir ? 0 : 1);
    if (dirs != 0) return dirs;
    int byName() => a.name.toLowerCase().compareTo(b.name.toLowerCase());
    var main = switch (sort.key) {
      SortKey.name => byName(),
      SortKey.status => itemStatus(a).rank.compareTo(itemStatus(b).rank),
      SortKey.size => a.bytes.compareTo(b.bytes),
      SortKey.date => _compareNullable(itemDateMs(a.entry), itemDateMs(b.entry)),
    };
    if (sort.descending) main = -main;
    return main != 0 ? main : byName();
  });
  return items;
}

/// null sorts before everything (Rust's `Option` ordering).
int _compareNullable(int? a, int? b) {
  if (a == null && b == null) return 0;
  if (a == null) return -1;
  if (b == null) return 1;
  return a.compareTo(b);
}

/// Title and text of the empty list for a filter.
(String, String) emptyText(Filter f) => switch (f) {
  Filter.all => ('Nothing here yet', ''),
  Filter.onComputer => (
    'No files on this computer',
    'Everything in this folder is kept only in Google Drive.',
  ),
  Filter.cloudOnly => (
    'No cloud-only files',
    'Files you free up or remove from this computer stay in Google Drive and show up here.',
  ),
  Filter.uploading => ('Nothing is uploading', 'Everything here has been uploaded.'),
  Filter.failed => ('No failed uploads', 'Every file here was uploaded successfully.'),
};

enum Keep {
  /// Not a single folder.
  unavailable,
  off,
  on,

  /// Kept because a parent folder is kept; can't be switched off here.
  inherited,
}

/// What the selected items allow.
class Caps {
  const Caps({
    this.count = 0,
    this.download = false,
    this.free = false,
    this.openDrive = false,
    this.show = false,
    this.retry = false,
    this.keep = Keep.unavailable,
  });

  final int count;
  final bool download;
  final bool free;
  final bool openDrive;
  final bool show;
  final bool retry;
  final Keep keep;
}

Caps capsFor(List<DirItem> items, Keep Function(String path) keepState) {
  var download = false, free = false, retry = false;
  for (final it in items) {
    final e = it.entry;
    final s = it.stats;
    if (e.isDir && s != null) {
      download |= s.cloudOnly > 0 || e.state == EntryState.cloudOnly;
      free |= s.local - s.waiting - s.failed > 0;
      retry |= s.failed > 0 || e.state == EntryState.error;
    } else {
      download |= e.state == EntryState.cloudOnly;
      free |= e.state == EntryState.synced;
      retry |= e.state == EntryState.error;
    }
  }
  var openDrive = false;
  var keep = Keep.unavailable;
  if (items.length == 1) {
    final only = items.first;
    openDrive = only.entry.driveId != null;
    if (only.isDir) keep = keepState(only.entry.path);
  }
  return Caps(
    count: items.length,
    download: download,
    free: free,
    openDrive: openDrive,
    show: items.isNotEmpty,
    retry: retry,
    keep: keep,
  );
}

/// How a folder is kept: itself, via a parent, or not.
Keep keepStateFor(List<String> keepList, String path) {
  bool trimmedEq(String k) => k.replaceAll(RegExp(r'^/+|/+$'), '') == path;
  if (keepList.any(trimmedEq)) return Keep.on;
  for (final k in keepList) {
    final key = k.replaceAll(RegExp(r'^/+|/+$'), '');
    if (path.startsWith('$key/')) return Keep.inherited;
  }
  return Keep.off;
}
