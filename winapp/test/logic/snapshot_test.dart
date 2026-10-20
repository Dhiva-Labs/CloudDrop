import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/providers.dart';
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
  CloudProvider provider = CloudProvider.google,
}) => Snapshot(
  provider: provider,
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

  // ---------- other providers ----------

  const webdav = CloudProvider(
    id: 'webdav',
    name: 'WebDAV',
    signIn: SignInKind.form,
    verifyByDownload: true,
    webLinks: false,
  );
  const oneDrive = CloudProvider(id: 'onedrive', name: 'OneDrive', signIn: SignInKind.oauth);

  test('Google Drive keeps every sentence it had', () {
    final notSignedIn = Snapshot(cfg: cfg, signedIn: false, daemon: snap(Phase.idle).daemon);
    expect(
      summarize(notSignedIn).detail,
      'CloudDrop needs your permission to upload to Google Drive.',
    );
    expect(
      summarize(snap(Phase.needsSignIn)).detail,
      "Google no longer accepts CloudDrop's sign-in, so uploads are on hold.",
    );
    expect(summarize(snap(Phase.driveFolderMissing)).line, 'Drive folder missing');
    expect(summarize(snap(Phase.starting)).detail, 'Connecting to Google Drive.');
    expect(
      summarize(snap(Phase.idle, counts: const Counts(failed: 1))).detail,
      'Everything else is safe in Google Drive.',
    );
    expect(summarize(const Snapshot()).detail, 'Set it up to start uploading to the cloud.');
  });

  test('other providers are named in every summary', () {
    expect(summarize(snap(Phase.offline, provider: oneDrive)).title, "Can't reach OneDrive");
    expect(summarize(snap(Phase.starting, provider: oneDrive)).detail, 'Connecting to OneDrive.');
    expect(
      summarize(snap(Phase.needsSignIn, provider: oneDrive)).detail,
      "OneDrive no longer accepts CloudDrop's sign-in, so uploads are on hold.",
    );
    final missing = summarize(snap(Phase.driveFolderMissing, provider: oneDrive));
    expect(missing.title, 'The CloudDrop folder is missing in OneDrive');
    expect(missing.line, 'Cloud folder missing');
    expect(
      summarize(
        snap(Phase.idle, counts: const Counts(synced: 2), cloudBytes: 2048, provider: oneDrive),
      ).detail,
      '2 files · 2.0 KB safe in OneDrive',
    );
    final oauth = Snapshot(cfg: cfg, signedIn: false, provider: oneDrive);
    expect(summarize(oauth).title, 'Sign in to OneDrive');
    expect(summarize(oauth).detail, 'CloudDrop needs your permission to upload to OneDrive.');
    final form = Snapshot(cfg: cfg, signedIn: false, provider: webdav);
    expect(summarize(form).title, 'Connect to WebDAV');
    expect(summarize(form).detail, 'CloudDrop needs your WebDAV details to upload.');
    expect(summarize(form).action, SummaryAction.signIn);
  });

  test('verifying: the phase, or a "Verifying" message while idle or waiting', () {
    final phase = snap(Phase.verifying, provider: webdav);
    expect(phase.verifying, isTrue);
    final v = summarize(phase);
    expect((v.title, v.line, v.icon), ('Verifying…', 'Verifying…', SummaryIcon.syncing));
    expect(v.detail, 'Making sure WebDAV holds identical copies before freeing up space.');
    final msg = summarize(snap(Phase.idle, message: 'verifying 3 files', provider: webdav));
    expect((msg.title, msg.detail), ('Verifying…', 'Verifying 3 files'));
    expect(snap(Phase.waiting, message: 'Verifying report.pdf').verifying, isTrue);
    expect(snap(Phase.uploading, message: 'Verifying report.pdf').verifying, isFalse);
    expect(snap(Phase.paused, message: 'Verifying report.pdf').verifying, isFalse);
    expect(snap(Phase.idle).verifying, isFalse);
    expect(const Snapshot().verifying, isFalse);
    expect(Phase.parse('verifying'), Phase.verifying);
    expect(Phase.parse('remote_folder_missing'), Phase.driveFolderMissing);
  });

  group('the cloud storage card', () {
    test('Google: quota, unlimited, or still asking (unchanged)', () {
      var c = cloudStorage(
        snap(
          Phase.idle,
          cloudBytes: 1024,
          quota: const Quota(limit: 4096, usage: 1024, usageInDrive: 0),
        ),
      );
      expect(
        (c.headline, c.note, c.fraction, c.muted),
        ('1.0 KB of 4.0 KB used', 'CloudDrop holds 1.0 KB', 0.25, false),
      );
      c = cloudStorage(snap(Phase.idle, quota: const Quota(usage: 0, usageInDrive: 0)));
      expect((c.headline, c.note), ('0 B used', 'CloudDrop holds 0 B · unlimited storage'));
      c = cloudStorage(snap(Phase.idle));
      expect((c.headline, c.muted), ('Checking…', true));
      c = cloudStorage(Snapshot(cfg: cfg, signedIn: true));
      expect(c.headline, 'Not connected');
    });

    test('a provider without a quota: "CloudDrop holds …"', () {
      const s3 = CloudProvider(id: 's3', name: 'S3', signIn: SignInKind.form, hasQuota: false);
      var c = cloudStorage(snap(Phase.idle, cloudBytes: 25 * 1024 * 1024, provider: s3));
      expect(
        (c.headline, c.note, c.fraction, c.muted),
        ('CloudDrop holds 25.0 MB', "S3 doesn't report free space", null, false),
      );
      // Nothing used of no limit is what a provider without a quota reports.
      c = cloudStorage(
        snap(
          Phase.idle,
          cloudBytes: 2048,
          quota: const Quota(usage: 0, usageInDrive: 0),
          provider: webdav,
        ),
      );
      expect(c.headline, 'CloudDrop holds 2.0 KB');
      c = cloudStorage(
        snap(
          Phase.idle,
          quota: const Quota(limit: 1024, usage: 512, usageInDrive: 0),
          provider: webdav,
        ),
      );
      expect(c.headline, '512 B of 1.0 KB used');
      c = cloudStorage(snap(Phase.idle, provider: oneDrive));
      expect(c.headline, 'Checking…', reason: 'OneDrive reports one, once asked');
    });
  });
}
