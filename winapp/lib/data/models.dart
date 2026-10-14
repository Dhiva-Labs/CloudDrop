/// Plain data types shared by the data layer, the logic and the UI.
///
/// These mirror `src/db.rs` / `src/control.rs` of the engine; see docs/DESIGN.md.
library;

import 'package:flutter/foundation.dart';

enum EntryKind { file, dir }

/// `entries.state` in the engine database.
enum EntryState {
  pending('pending'),
  uploading('uploading'),
  synced('synced'),
  cloudOnly('cloud_only'),
  error('error');

  const EntryState(this.wire);

  /// The value stored in the database.
  final String wire;

  static EntryState? parse(String? s) {
    for (final v in values) {
      if (v.wire == s) return v;
    }
    return null;
  }
}

/// One tracked file or directory.
@immutable
class Entry {
  const Entry({
    required this.path,
    required this.kind,
    required this.state,
    this.driveId,
    this.cloudName,
    this.cloudParent,
    this.md5,
    this.cloudSize,
    this.localSize,
    this.localMtimeNs,
    this.inode,
    this.error,
    this.uploadedAt,
  });

  /// Relative to the sync folder, '/' separated, no leading or trailing slash.
  final String path;
  final EntryKind kind;
  final EntryState state;
  final String? driveId;
  final String? cloudName;
  final String? cloudParent;
  final String? md5;
  final int? cloudSize;
  final int? localSize;
  final int? localMtimeNs;
  final int? inode;
  final String? error;

  /// Unix ms the current Drive content was uploaded.
  final int? uploadedAt;

  bool get isDir => kind == EntryKind.dir;

  String get name => nameOf(path);

  String get parent => parentOf(path);

  /// Best-known size: the Drive copy for cloud-only files, otherwise the local file.
  int get size => state == EntryState.cloudOnly ? (cloudSize ?? 0) : (localSize ?? cloudSize ?? 0);

  @override
  bool operator ==(Object other) =>
      other is Entry &&
      other.path == path &&
      other.kind == kind &&
      other.state == state &&
      other.driveId == driveId &&
      other.cloudName == cloudName &&
      other.cloudParent == cloudParent &&
      other.md5 == md5 &&
      other.cloudSize == cloudSize &&
      other.localSize == localSize &&
      other.localMtimeNs == localMtimeNs &&
      other.inode == inode &&
      other.error == error &&
      other.uploadedAt == uploadedAt;

  @override
  int get hashCode => Object.hash(
    path,
    kind,
    state,
    driveId,
    md5,
    cloudSize,
    localSize,
    localMtimeNs,
    error,
    uploadedAt,
  );

  @override
  String toString() => 'Entry($path, ${kind.name}, ${state.wire})';
}

/// Totals over the files beneath a folder.
@immutable
class FolderStats {
  const FolderStats({
    this.files = 0,
    this.bytes = 0,
    this.local = 0,
    this.cloudOnly = 0,
    this.waiting = 0,
    this.failed = 0,
    this.cloudOnlyBytes = 0,
  });

  final int files;

  /// Size of all files, wherever they are.
  final int bytes;

  /// Files present on this computer (uploaded or not).
  final int local;
  final int cloudOnly;
  final int waiting;
  final int failed;

  /// Bytes kept only in Drive.
  final int cloudOnlyBytes;

  @override
  bool operator ==(Object other) =>
      other is FolderStats &&
      other.files == files &&
      other.bytes == bytes &&
      other.local == local &&
      other.cloudOnly == cloudOnly &&
      other.waiting == waiting &&
      other.failed == failed &&
      other.cloudOnlyBytes == cloudOnlyBytes;

  @override
  int get hashCode => Object.hash(files, bytes, local, cloudOnly, waiting, failed, cloudOnlyBytes);

  @override
  String toString() =>
      'FolderStats(files: $files, bytes: $bytes, local: $local, cloudOnly: $cloudOnly, '
      'waiting: $waiting, failed: $failed, cloudOnlyBytes: $cloudOnlyBytes)';
}

/// A row for the file browser.
@immutable
class DirItem {
  const DirItem({required this.entry, required this.bytes, this.stats});

  /// A file row.
  factory DirItem.file(Entry e) => DirItem(entry: e, bytes: e.size);

  final Entry entry;

  /// Files: the file size. Folders: total size beneath.
  final int bytes;

  /// Folders only.
  final FolderStats? stats;

  String get name => entry.name;
  String get path => entry.path;
  bool get isDir => entry.isDir;

  @override
  bool operator ==(Object other) =>
      other is DirItem && other.entry == entry && other.bytes == bytes && other.stats == stats;

  @override
  int get hashCode => Object.hash(entry, bytes, stats);
}

/// What the file browser asks for.
enum Filter {
  all('All files'),
  onComputer('On this computer'),
  cloudOnly('Cloud-only'),
  uploading('Uploading'),
  failed('Failed');

  const Filter(this.label);
  final String label;
}

sealed class Scope {
  const Scope();
}

class DirScope extends Scope {
  const DirScope(this.dir);
  final String dir;

  @override
  bool operator ==(Object other) => other is DirScope && other.dir == dir;

  @override
  int get hashCode => dir.hashCode;
}

class SearchScope extends Scope {
  const SearchScope(this.needle);
  final String needle;

  @override
  bool operator ==(Object other) => other is SearchScope && other.needle == needle;

  @override
  int get hashCode => needle.hashCode;
}

@immutable
class Request {
  const Request(this.scope, this.filter);
  final Scope scope;
  final Filter filter;

  /// Several rows can come from different folders, so show where they live.
  bool get isFlat => scope is SearchScope || filter != Filter.all;

  @override
  bool operator ==(Object other) =>
      other is Request && other.scope == scope && other.filter == filter;

  @override
  int get hashCode => Object.hash(scope, filter);
}

@immutable
class Listing {
  const Listing({this.items = const [], this.truncated = false, this.stats, this.dir = ''});

  final List<DirItem> items;

  /// More matched than are shown.
  final bool truncated;

  /// Totals for the folder being viewed (searches: none).
  final FolderStats? stats;

  /// The folder actually shown (a vanished folder falls back to its nearest parent).
  final String dir;
}

enum ActivityKind {
  uploaded('uploaded'),
  updated('updated'),
  moved('moved'),
  keptInCloud('kept_in_cloud'),
  freed('freed'),
  autoFreed('auto_freed'),
  restored('restored'),
  failed('failed');

  const ActivityKind(this.wire);
  final String wire;

  static ActivityKind? parse(String? s) {
    for (final v in values) {
      if (v.wire == s) return v;
    }
    return null;
  }
}

/// One line of history: what happened to which path, and when.
@immutable
class Activity {
  const Activity({
    required this.id,
    required this.atMs,
    required this.kind,
    required this.path,
    this.bytes,
    this.detail,
  });

  final int id;
  final int atMs;
  final ActivityKind kind;
  final String path;
  final int? bytes;

  /// Extra text: the old path for moves, the error for failures.
  final String? detail;

  @override
  bool operator ==(Object other) =>
      other is Activity &&
      other.id == id &&
      other.atMs == atMs &&
      other.kind == kind &&
      other.path == path &&
      other.bytes == bytes &&
      other.detail == detail;

  @override
  int get hashCode => Object.hash(id, atMs, kind, path, bytes, detail);
}

/// `kind`/`state` counts from `SELECT kind, state, count(*) ... GROUP BY`.
@immutable
class StateCount {
  const StateCount(this.kind, this.state, this.count);
  final String kind;
  final String state;
  final int count;
}

// ---------- daemon status (status.json) ----------

/// `phase` of status.json.
enum Phase {
  starting('starting'),
  idle('idle'),
  waiting('waiting'),
  uploading('uploading'),
  paused('paused'),
  offline('offline'),
  needsSignIn('needs_sign_in'),
  driveFolderMissing('drive_folder_missing');

  const Phase(this.wire);
  final String wire;

  static Phase? parse(String? s) {
    for (final v in values) {
      if (v.wire == s) return v;
    }
    return null;
  }
}

@immutable
class Upload {
  const Upload({required this.path, required this.sent, required this.total});
  final String path;
  final int sent;
  final int total;

  factory Upload.fromJson(Map<String, dynamic> j) => Upload(
    path: j['path'] as String? ?? '',
    sent: _int(j['sent']) ?? 0,
    total: _int(j['total']) ?? 0,
  );

  double get fraction => total > 0 ? (sent / total).clamp(0.0, 1.0) : 0.0;

  @override
  bool operator ==(Object other) =>
      other is Upload && other.path == path && other.sent == sent && other.total == total;

  @override
  int get hashCode => Object.hash(path, sent, total);
}

/// Google Drive storage of the signed-in account, in bytes.
@immutable
class Quota {
  const Quota({this.limit, required this.usage, required this.usageInDrive});

  /// null = unlimited.
  final int? limit;

  /// Everything counted against the limit (Drive, Gmail, Photos).
  final int usage;
  final int usageInDrive;

  factory Quota.fromJson(Map<String, dynamic> j) => Quota(
    limit: _int(j['limit']),
    usage: _int(j['usage']) ?? 0,
    usageInDrive: _int(j['usage_in_drive']) ?? 0,
  );

  @override
  bool operator ==(Object other) =>
      other is Quota &&
      other.limit == limit &&
      other.usage == usage &&
      other.usageInDrive == usageInDrive;

  @override
  int get hashCode => Object.hash(limit, usage, usageInDrive);
}

/// What the engine is doing, from `status.json`.
@immutable
class DaemonStatus {
  const DaemonStatus({
    required this.pid,
    required this.updatedMs,
    required this.phase,
    this.message,
    this.uploads = const [],
    this.waiting = 0,
    this.quota,
  });

  final int pid;
  final int updatedMs;
  final Phase phase;
  final String? message;
  final List<Upload> uploads;
  final int waiting;
  final Quota? quota;

  static DaemonStatus? fromJson(Object? json) {
    if (json is! Map) return null;
    final j = json.cast<String, dynamic>();
    final phase = Phase.parse(j['phase'] as String?);
    if (phase == null) return null;
    final uploads = <Upload>[];
    final raw = j['uploads'];
    if (raw is List) {
      for (final u in raw) {
        if (u is Map) uploads.add(Upload.fromJson(u.cast<String, dynamic>()));
      }
    }
    final q = j['quota'];
    return DaemonStatus(
      pid: _int(j['pid']) ?? 0,
      updatedMs: _int(j['updated_ms']) ?? 0,
      phase: phase,
      message: j['message'] as String?,
      uploads: uploads,
      waiting: _int(j['waiting']) ?? 0,
      quota: q is Map ? Quota.fromJson(q.cast<String, dynamic>()) : null,
    );
  }

  /// Heartbeat younger than 10 s from a process that still exists.
  bool isLive(int nowMs, {bool Function(int pid)? pidAlive}) =>
      nowMs - updatedMs < 10000 && (pidAlive?.call(pid) ?? true);

  @override
  bool operator ==(Object other) =>
      other is DaemonStatus &&
      other.pid == pid &&
      other.updatedMs == updatedMs &&
      other.phase == phase &&
      other.message == message &&
      listEquals(other.uploads, uploads) &&
      other.waiting == waiting &&
      other.quota == quota;

  @override
  int get hashCode =>
      Object.hash(pid, updatedMs, phase, message, Object.hashAll(uploads), waiting, quota);
}

int? _int(Object? v) {
  if (v is int) return v;
  if (v is num) return v.toInt();
  if (v is String) return int.tryParse(v);
  return null;
}

/// Shared by [Entry] and status code: last path segment.
String nameOf(String rel) {
  final i = rel.lastIndexOf('/');
  return i < 0 ? rel : rel.substring(i + 1);
}

/// Parent of a relative entry path ("" for top-level entries).
String parentOf(String rel) {
  final i = rel.lastIndexOf('/');
  return i < 0 ? '' : rel.substring(0, i);
}

String joinRel(String parent, String name) => parent.isEmpty ? name : '$parent/$name';
