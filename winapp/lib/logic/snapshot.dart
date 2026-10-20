/// One read of everything the Home screen shows, and its wording
/// (`gui/src/snapshot.rs`).
library;

import 'package:flutter/foundation.dart';

import '../data/engine_api.dart';
import '../data/models.dart';
import '../data/providers.dart';
import 'fmt.dart';

@immutable
class Counts {
  const Counts({this.synced = 0, this.waiting = 0, this.cloudOnly = 0, this.failed = 0});

  final int synced;
  final int waiting;
  final int cloudOnly;
  final int failed;

  /// File counts per (kind, state) from the database.
  factory Counts.fromRows(Iterable<StateCount> rows) {
    var synced = 0, waiting = 0, cloudOnly = 0, failed = 0;
    for (final r in rows) {
      if (r.kind != 'file') continue;
      switch (r.state) {
        case 'synced':
          synced += r.count;
        case 'pending' || 'uploading':
          waiting += r.count;
        case 'cloud_only':
          cloudOnly += r.count;
        case 'error':
          failed += r.count;
      }
    }
    return Counts(synced: synced, waiting: waiting, cloudOnly: cloudOnly, failed: failed);
  }

  @override
  bool operator ==(Object other) =>
      other is Counts &&
      other.synced == synced &&
      other.waiting == waiting &&
      other.cloudOnly == cloudOnly &&
      other.failed == failed;

  @override
  int get hashCode => Object.hash(synced, waiting, cloudOnly, failed);
}

@immutable
class Snapshot {
  const Snapshot({
    this.cfg,
    this.signedIn = false,
    this.daemon,
    this.paused = false,
    this.counts = const Counts(),
    this.cloudBytes = 0,
    this.cloudOnlyBytes = 0,
    this.root = const FolderStats(),
    this.rootId,
    this.rate,
    this.provider = CloudProvider.google,
  });

  /// null until the app is set up.
  final AppConfig? cfg;
  final bool signedIn;

  /// Present only while the engine is alive.
  final DaemonStatus? daemon;
  final bool paused;
  final Counts counts;
  final int cloudBytes;
  final int cloudOnlyBytes;

  /// Totals over everything in the CloudDrop folder.
  final FolderStats root;
  final String? rootId;

  /// Upload speed in bytes per second, once it has been measured.
  final double? rate;

  /// The cloud it uploads to.
  final CloudProvider provider;

  /// Storage of the cloud account; null until the engine has asked.
  Quota? get quota => daemon?.quota;

  /// The engine is proving cloud copies by downloading them (providers without a hash).
  bool get verifying {
    final d = daemon;
    if (d == null) return false;
    if (d.phase == Phase.verifying) return true;
    return (d.phase == Phase.idle || d.phase == Phase.waiting) &&
        (d.message ?? '').toLowerCase().startsWith('verifying');
  }

  bool get ready => cfg != null && signedIn;

  /// (bytes sent, bytes total) across the uploads running now.
  (int, int) get progress {
    final d = daemon;
    if (d == null) return (0, 0);
    var sent = 0, total = 0;
    for (final u in d.uploads) {
      sent += u.sent;
      total += u.total;
    }
    return (sent, total);
  }

  /// Bytes still to send: the rest of the uploads running now and every file waiting.
  int get remaining {
    final (sent, _) = progress;
    final left = root.waitingBytes - sent;
    return left < 0 ? 0 : left;
  }

  @override
  bool operator ==(Object other) =>
      other is Snapshot &&
      other.cfg == cfg &&
      other.signedIn == signedIn &&
      other.daemon == daemon &&
      other.paused == paused &&
      other.counts == counts &&
      other.cloudBytes == cloudBytes &&
      other.cloudOnlyBytes == cloudOnlyBytes &&
      other.root == root &&
      other.rootId == rootId &&
      other.rate == rate &&
      other.provider == provider;

  @override
  int get hashCode => Object.hash(
    cfg,
    signedIn,
    daemon,
    paused,
    counts,
    cloudBytes,
    cloudOnlyBytes,
    root,
    rootId,
    rate,
    provider,
  );
}

enum SummaryAction { setup, signIn, startService, resume }

/// Which picture the hero shows.
enum SummaryIcon { idle, syncing, paused, attention }

@immutable
class Summary {
  const Summary({
    required this.icon,
    required this.title,
    required this.detail,
    required this.line,
    this.action,
    this.fraction,
    this.speed,
  });

  final SummaryIcon icon;
  final String title;
  final String detail;

  /// How far the uploads running now are (0..1), while uploading.
  final double? fraction;

  /// "2.4 MB/s · about 3 minutes left", once the speed is known.
  final String? speed;

  /// Short line for the window title area / tooltips.
  final String line;
  final SummaryAction? action;

  @override
  bool operator ==(Object other) =>
      other is Summary &&
      other.icon == icon &&
      other.title == title &&
      other.detail == detail &&
      other.line == line &&
      other.action == action &&
      other.fraction == fraction &&
      other.speed == speed;

  @override
  int get hashCode => Object.hash(icon, title, detail, line, action, fraction, speed);
}

Summary summarize(Snapshot s) {
  final p = s.provider;
  final cloud = p.name;
  if (s.cfg == null) {
    return const Summary(
      icon: SummaryIcon.attention,
      title: "CloudDrop isn't set up yet",
      detail: 'Set it up to start uploading to the cloud.',
      line: 'Not set up',
      action: SummaryAction.setup,
    );
  }
  if (!s.signedIn) {
    final form = p.signIn == SignInKind.form;
    return Summary(
      icon: SummaryIcon.attention,
      title: p.isGoogle
          ? 'Sign in to Google'
          : form
          ? 'Connect to $cloud'
          : 'Sign in to $cloud',
      detail: form
          ? 'CloudDrop needs your $cloud details to upload.'
          : 'CloudDrop needs your permission to upload to $cloud.',
      line: 'Sign-in needed',
      action: SummaryAction.signIn,
    );
  }
  final d = s.daemon;
  if (d == null) {
    return const Summary(
      icon: SummaryIcon.attention,
      title: "CloudDrop isn't running",
      detail: 'Nothing is being uploaded until it starts.',
      line: 'Not running',
      action: SummaryAction.startService,
    );
  }
  final message = d.message ?? '';
  if (s.verifying) return _verifying(s, message);
  switch (d.phase) {
    case Phase.paused:
      return Summary(
        icon: SummaryIcon.paused,
        title: 'Uploads are paused',
        detail: d.waiting > 0
            ? '${files(d.waiting)} waiting. Unfinished uploads continue where they stopped.'
            : 'New files will wait until you resume.',
        line: 'Paused',
        action: SummaryAction.resume,
      );
    case Phase.needsSignIn:
      return Summary(
        icon: SummaryIcon.attention,
        title: 'Sign in again',
        detail:
            "${p.isGoogle ? 'Google' : cloud} no longer accepts CloudDrop's sign-in, "
            'so uploads are on hold.',
        line: 'Sign-in needed',
        action: SummaryAction.signIn,
      );
    case Phase.driveFolderMissing:
      return Summary(
        icon: SummaryIcon.attention,
        title: 'The CloudDrop folder is missing in $cloud',
        detail: _capitalize(message),
        line: p.isGoogle ? 'Drive folder missing' : 'Cloud folder missing',
      );
    case Phase.offline:
      return Summary(
        icon: SummaryIcon.attention,
        title: "Can't reach $cloud",
        detail: '${files(d.waiting)} waiting. CloudDrop keeps retrying on its own.',
        line: 'Offline — retrying',
      );
    case Phase.starting:
      return Summary(
        icon: SummaryIcon.syncing,
        title: 'Starting…',
        detail: 'Connecting to $cloud.',
        line: 'Starting',
      );
    case Phase.verifying:
      return _verifying(s, message);
    case Phase.uploading || Phase.waiting:
      final (sent, total) = s.progress;
      final pct = total > 0 ? (sent * 100) ~/ total : 0;
      final n = d.waiting < 1 ? 1 : d.waiting;
      return Summary(
        icon: SummaryIcon.syncing,
        title: 'Uploading ${files(n)}',
        detail: total > 0
            ? '${humanBytes(sent)} of ${humanBytes(total)} sent for the current files.'
            : 'Waiting for a moment to upload.',
        line: total > 0 ? 'Uploading ${files(n)} — $pct%' : '${files(d.waiting)} waiting',
        fraction: total > 0 ? sent / total : null,
        speed: _speed(s),
      );
    case Phase.idle:
      if (s.counts.failed > 0) {
        return Summary(
          icon: SummaryIcon.attention,
          title: 'Some files need attention',
          detail: 'Everything else is safe in $cloud.',
          line: '${files(s.counts.failed)} failed',
        );
      }
      final n = s.counts.synced + s.counts.cloudOnly;
      return Summary(
        icon: SummaryIcon.idle,
        title: 'Everything is uploaded',
        detail: n == 0
            ? 'Put files in your CloudDrop folder to back them up.'
            : '${files(n)} · ${humanBytes(s.cloudBytes)} safe in $cloud',
        line: 'Up to date',
      );
  }
}

/// "Verifying…": the engine downloads cloud copies once to prove them before freeing space.
Summary _verifying(Snapshot s, String message) => Summary(
  icon: SummaryIcon.syncing,
  title: 'Verifying…',
  detail: message.isEmpty
      ? 'Making sure ${s.provider.name} holds identical copies before freeing up space.'
      : _capitalize(message),
  line: 'Verifying…',
);

/// What Home's cloud storage card says.
typedef StorageText = ({String headline, String note, double? fraction, bool muted});

StorageText cloudStorage(Snapshot s) {
  final holds = 'CloudDrop holds ${humanBytes(s.cloudBytes)}';
  final q = s.quota;
  final limit = q?.limit;
  if (q != null && limit != null && limit > 0) {
    return (
      headline: '${humanBytes(q.usage)} of ${humanBytes(limit)} used',
      note: holds,
      fraction: q.usage / limit,
      muted: false,
    );
  }
  // Providers without a quota (S3) report none at all, or nothing used of no limit.
  final p = s.provider;
  final noQuota = p.hasQuota == false || (!p.isGoogle && q != null && q.usage == 0);
  if (noQuota) {
    return (
      headline: holds,
      note: "${p.name} doesn't report free space",
      fraction: null,
      muted: false,
    );
  }
  if (q != null) {
    return (
      headline: '${humanBytes(q.usage)} used',
      note: '$holds · unlimited storage',
      fraction: null,
      muted: false,
    );
  }
  return (
    headline: s.daemon != null ? 'Checking…' : 'Not connected',
    note: holds,
    fraction: null,
    muted: true,
  );
}

/// "2.4 MB/s · about 3 minutes left"; just the speed when nothing is known to be waiting.
String? _speed(Snapshot s) {
  final rate = s.rate;
  if (rate == null || rate < 1) return null;
  final left = s.remaining;
  if (left == 0) return speed(rate);
  return '${speed(rate)} · ${timeLeft((left / rate).ceil())}';
}

String _capitalize(String s) => s.isEmpty ? s : s[0].toUpperCase() + s.substring(1);
