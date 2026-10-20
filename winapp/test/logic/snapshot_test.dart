import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/snapshot.dart';
import 'package:flutter_test/flutter_test.dart';

const cfg = AppConfig(
  syncDir: '/home/me/CloudDrop',
  driveFolder: 'CloudDrop',
  clientId: 'x.apps.googleusercontent.com',
  hasClientSecret: true,
);

Snapshot snap(
  Phase phase, {
  Counts counts = const Counts(),
  int waiting = 0,
  String? message,
  List<Upload> uploads = const [],
  Quota? quota,
  int cloudBytes = 0,
  double? rate,
  int waitingBytes = 0,
}) => Snapshot(
  cfg: cfg,
  signedIn: true,
  daemon: DaemonStatus(
    pid: 1,
    updatedMs: 0,
    phase: phase,
    waiting: waiting,
    message: message,
    uploads: uploads,
    quota: quota,
  ),
  counts: counts,
  cloudBytes: cloudBytes,
  rootId: 'R',
  rate: rate,
  root: FolderStats(waitingBytes: waitingBytes),
);

void main() {
  test('idle: everything uploaded', () {
    final s = summarize(snap(Phase.idle));
    expect(s.icon, SummaryIcon.idle);
    expect(s.title, 'Everything is uploaded');
    expect(s.detail, 'Put files in your CloudDrop folder to back them up.');
    expect(s.line, 'Up to date');
  });

  test('idle subtitle counts files in Drive', () {
    final s = summarize(
      snap(Phase.idle, counts: const Counts(synced: 2, cloudOnly: 1), cloudBytes: 3 * 1024),
    );
    expect(s.detail, '3 files · 3.0 KB safe in Google Drive');
  });

  test('failures need attention', () {
    final s = summarize(snap(Phase.idle, counts: const Counts(failed: 2, synced: 5)));
    expect(s.icon, SummaryIcon.attention);
    expect(s.title, 'Some files need attention');
    expect(s.detail, 'Everything else is safe in Google Drive.');
    expect(s.line, '2 files failed');
  });

  test('uploading shows a percentage', () {
    final s = summarize(
      snap(Phase.uploading, waiting: 3, uploads: const [Upload(path: 'a', sent: 50, total: 200)]),
    );
    expect(s.title, 'Uploading 3 files');
    expect(s.detail, '50 B of 200 B sent for the current files.');
    expect(s.line, 'Uploading 3 files — 25%');
    expect(s.icon, SummaryIcon.syncing);
    expect(s.action, isNull);
  });

  test('waiting with nothing sent yet', () {
    final s = summarize(snap(Phase.waiting, waiting: 4));
    expect(s.title, 'Uploading 4 files');
    expect(s.detail, 'Waiting for a moment to upload.');
    expect(s.line, '4 files waiting');
    expect(summarize(snap(Phase.uploading)).title, 'Uploading 1 file');
  });

  test('paused offers to resume', () {
    var s = summarize(snap(Phase.paused, waiting: 2));
    expect(s.title, 'Uploads are paused');
    expect(s.detail, '2 files waiting. Unfinished uploads continue where they stopped.');
    expect(s.action, SummaryAction.resume);
    expect(s.icon, SummaryIcon.paused);
    s = summarize(snap(Phase.paused));
    expect(s.detail, 'New files will wait until you resume.');
  });

  test('offline, sign-in and missing Drive folder', () {
    var s = summarize(snap(Phase.offline, waiting: 1));
    expect(s.title, "Can't reach Google Drive");
    expect(s.detail, '1 file waiting. CloudDrop keeps retrying on its own.');
    expect(s.line, 'Offline — retrying');
    s = summarize(snap(Phase.needsSignIn));
    expect(s.title, 'Sign in again');
    expect(s.action, SummaryAction.signIn);
    s = summarize(snap(Phase.driveFolderMissing, message: 'the folder was trashed'));
    expect(s.title, 'The CloudDrop folder is missing in Google Drive');
    expect(s.detail, 'The folder was trashed');
    expect(summarize(snap(Phase.starting)).title, 'Starting…');
  });

  test('set-up states come first', () {
    var s = snap(Phase.idle);
    final notSignedIn = Snapshot(cfg: cfg, signedIn: false, daemon: s.daemon);
    expect(summarize(notSignedIn).action, SummaryAction.signIn);
    expect(summarize(notSignedIn).title, 'Sign in to Google');
    expect(summarize(const Snapshot()).action, SummaryAction.setup);
    expect(summarize(const Snapshot()).title, "CloudDrop isn't set up yet");
    final notRunning = Snapshot(cfg: cfg, signedIn: true);
    expect(summarize(notRunning).action, SummaryAction.startService);
    expect(summarize(notRunning).title, "CloudDrop isn't running");
    s = snap(Phase.idle);
    expect(s.ready, isTrue);
    expect(notSignedIn.ready, isFalse);
  });

  test('quota comes from the daemon', () {
    expect(snap(Phase.idle).quota, isNull);
    const q = Quota(limit: 15, usage: 3, usageInDrive: 2);
    expect(snap(Phase.idle, quota: q).quota, q);
    expect(const Snapshot().quota, isNull);
  });

  test('progress adds up the current uploads', () {
    final s = snap(
      Phase.uploading,
      uploads: const [
        Upload(path: 'a', sent: 10, total: 100),
        Upload(path: 'b', sent: 5, total: 50),
      ],
    );
    expect(s.progress, (15, 150));
    expect(const Snapshot().progress, (0, 0));
  });

  test('counts from database rows only count files', () {
    final c = Counts.fromRows(const [
      StateCount('file', 'synced', 4),
      StateCount('file', 'pending', 2),
      StateCount('file', 'uploading', 1),
      StateCount('file', 'cloud_only', 3),
      StateCount('file', 'error', 5),
      StateCount('dir', 'synced', 9),
    ]);
    expect((c.synced, c.waiting, c.cloudOnly, c.failed), (4, 3, 3, 5));
  });

  test('uploading: progress of the current files, then speed and time left', () {
    const up = [Upload(path: 'a', sent: 1024 * 1024, total: 4 * 1024 * 1024)];
    final measuring = summarize(snap(Phase.uploading, waiting: 2, uploads: up));
    expect(measuring.fraction, 0.25);
    expect(measuring.speed, isNull);
    final known = summarize(
      snap(
        Phase.uploading,
        waiting: 2,
        uploads: up,
        rate: 512 * 1024,
        waitingBytes: 4 * 1024 * 1024 + 300 * 1024 * 1024,
      ),
    );
    expect(known.speed, '512.0 KB/s · about 10 minutes left');
    final nothingQueued = summarize(snap(Phase.uploading, uploads: up, rate: 512 * 1024));
    expect(nothingQueued.speed, '512.0 KB/s');
  });

  test('idle has no progress', () {
    final s = summarize(snap(Phase.idle, rate: 1000));
    expect(s.fraction, isNull);
    expect(s.speed, isNull);
  });
}
