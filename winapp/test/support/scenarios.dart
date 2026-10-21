/// Believable CloudDrop folders for screenshots and tests.
library;

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/data/providers.dart';

import 'fakes.dart';
import 'harness.dart';
import 'test_db.dart';

const int kb = 1024;
const int mb = 1024 * 1024;
const int gb = 1024 * 1024 * 1024;

// ---------- providers, as `clouddrop providers --json` will list them ----------

const oneDrive = CloudProvider(id: 'onedrive', name: 'OneDrive', signIn: SignInKind.oauth);
const dropbox = CloudProvider(id: 'dropbox', name: 'Dropbox', signIn: SignInKind.oauth);
const nextcloud = CloudProvider(id: 'nextcloud', name: 'Nextcloud', signIn: SignInKind.nextcloud);
const webdav = CloudProvider(
  id: 'webdav',
  name: 'WebDAV',
  signIn: SignInKind.form,
  verifyByDownload: true,
  webLinks: false,
  fields: [
    ProviderField(
      key: 'url',
      label: 'Server address',
      kind: FieldKind.url,
      required: true,
      hint: 'https://cloud.example.com/remote.php/dav/files/me',
    ),
    ProviderField(key: 'user', label: 'User name', required: true),
    ProviderField(
      key: 'password',
      label: 'Password',
      kind: FieldKind.secret,
      required: true,
      hint: 'An app password, if your server offers them.',
    ),
  ],
);
const s3 = CloudProvider(
  id: 's3',
  name: 'S3',
  signIn: SignInKind.form,
  webLinks: false,
  hasQuota: false,
  fields: [
    ProviderField(
      key: 'endpoint',
      label: 'Endpoint',
      kind: FieldKind.url,
      required: true,
      hint: 'https://s3.eu-central-1.amazonaws.com',
    ),
    ProviderField(key: 'bucket', label: 'Bucket', required: true),
    ProviderField(key: 'prefix', label: 'Folder in the bucket', hint: 'Leave empty for the top'),
    ProviderField(key: 'access_key_id', label: 'Access key ID', required: true),
    ProviderField(
      key: 'secret_access_key',
      label: 'Secret access key',
      kind: FieldKind.secret,
      required: true,
    ),
  ],
);
const sftp = CloudProvider(
  id: 'sftp',
  name: 'SFTP',
  signIn: SignInKind.form,
  verifyByDownload: true,
  webLinks: false,
  fields: [
    ProviderField(key: 'host', label: 'Server', required: true, hint: 'nas.local'),
    ProviderField(key: 'port', label: 'Port', kind: FieldKind.number, hint: 'Usually 22'),
    ProviderField(key: 'user', label: 'User name', required: true),
    ProviderField(
      key: 'key_file',
      label: 'SSH key',
      kind: FieldKind.file,
      hint: '~/.ssh/id_ed25519',
    ),
  ],
);

/// Every provider, in the order the CLI lists them.
const List<CloudProvider> allProviders = [
  CloudProvider.google,
  oneDrive,
  dropbox,
  nextcloud,
  webdav,
  s3,
  sftp,
];

/// A folder with a bit of everything: nested folders, files in every state, a pinned folder.
void fillBusyFolder(TestDb db) {
  db.meta('root_id', 'ROOT-DRIVE-ID');

  db.dir('Documents');
  db.file(
    'Documents/Budget 2026.xlsx',
    size: 48 * kb,
    mtimeMs: ago(hours: 3),
    uploadedAt: ago(hours: 3),
  );
  db.file(
    'Documents/Resume.pdf',
    size: 212 * kb,
    mtimeMs: ago(days: 12),
    uploadedAt: ago(days: 12),
  );
  db.file(
    'Documents/Lease agreement.docx',
    size: 96 * kb,
    mtimeMs: ago(days: 80),
    uploadedAt: ago(days: 80),
  );
  db.dir('Documents/Invoices');
  db.file(
    'Documents/Invoices/Invoice 0042.pdf',
    size: 74 * kb,
    mtimeMs: ago(days: 40),
    uploadedAt: ago(days: 40),
  );
  db.file(
    'Documents/Invoices/Invoice 0043.pdf',
    state: 'cloud_only',
    cloudSize: 81 * kb,
    uploadedAt: ago(days: 33),
  );
  db.file(
    'Documents/Invoices/Invoice 0044.pdf',
    state: 'cloud_only',
    cloudSize: 79 * kb,
    uploadedAt: ago(days: 12),
  );

  db.dir('Photos');
  db.dir('Photos/Goa trip');
  for (var i = 1; i <= 4; i++) {
    final cloud = i > 2;
    db.file(
      'Photos/Goa trip/IMG_${2400 + i}.jpg',
      state: cloud ? 'cloud_only' : 'synced',
      size: (3 + i) * mb ~/ 2,
      cloudSize: (3 + i) * mb ~/ 2,
      mtimeMs: ago(days: 20 + i),
      uploadedAt: ago(days: 20 + i),
    );
  }
  db.file(
    'Photos/IMG_20261012_184501.jpg',
    state: 'uploading',
    size: 9800 * kb,
    mtimeMs: ago(minutes: 2),
  );
  db.file(
    'Photos/IMG_20261012_184733.jpg',
    state: 'pending',
    size: 7400 * kb,
    mtimeMs: ago(minutes: 1),
  );

  db.dir('Videos');
  db.file(
    'Videos/Family reunion.mp4',
    state: 'cloud_only',
    cloudSize: 1288 * mb,
    uploadedAt: ago(days: 60),
  );
  db.file(
    'Videos/Screen recording.mkv',
    state: 'cloud_only',
    cloudSize: 812 * mb,
    uploadedAt: ago(days: 45),
  );

  db.dir('Work');
  db.file(
    'Work/Project plan.docx',
    size: 132 * kb,
    mtimeMs: ago(days: 1),
    uploadedAt: ago(days: 1),
  );
  db.file('Work/Roadmap.pptx', size: 4 * mb, mtimeMs: ago(days: 5), uploadedAt: ago(days: 5));

  db.file('setup-guide.md', size: 6 * kb, mtimeMs: ago(days: 3), uploadedAt: ago(days: 3));
  db.file('backup-2026-10.zip', state: 'uploading', size: 410 * mb, mtimeMs: ago(minutes: 6));
  db.file('notes.txt', state: 'pending', size: 2 * kb, mtimeMs: ago(minutes: 1));
  db.file(
    'Quarterly report.pdf',
    state: 'error',
    size: 1536 * kb,
    mtimeMs: ago(hours: 20),
    error: 'Google Drive storage is full (403 storageQuotaExceeded)',
  );
  db.file(
    'Archive.7z',
    state: 'error',
    size: 22 * mb,
    mtimeMs: ago(days: 2),
    error: 'The file is larger than Drive accepts',
  );
}

/// The history, inserted oldest first (ids grow with time, like the engine's).
void fillActivity(TestDb db) {
  final rows = <({String kind, String path, int at, int? bytes, String? detail})>[];
  void a(String kind, String path, int at, {int? bytes, String? detail}) =>
      rows.add((kind: kind, path: path, at: at, bytes: bytes, detail: detail));
  a('uploaded', 'Documents/Budget 2026.xlsx', ago(hours: 3), bytes: 48 * kb);
  a('updated', 'Work/Project plan.docx', ago(hours: 5), bytes: 132 * kb);
  a(
    'failed',
    'Quarterly report.pdf',
    ago(hours: 20),
    bytes: 1536 * kb,
    detail: 'Google Drive storage is full (403 storageQuotaExceeded)\nmore',
  );
  a('moved', 'Photos/Goa trip/IMG_2403.jpg', ago(hours: 27), detail: 'Photos/IMG_2403.jpg');
  a('moved', 'Documents/Resume.pdf', ago(hours: 28), detail: 'Documents/CV.pdf');
  a('freed', 'Videos/Family reunion.mp4', ago(hours: 30), bytes: 1288 * mb);
  a('auto_freed', 'Documents/Invoices/Invoice 0043.pdf', ago(hours: 32), bytes: 81 * kb);
  a('restored', 'Photos/Goa trip/IMG_2401.jpg', ago(days: 2, hours: 1), bytes: 2 * mb);
  a('kept_in_cloud', 'Old stuff/draft.txt', ago(days: 3), bytes: 3 * kb);
  a('uploaded', 'setup-guide.md', ago(days: 3, hours: 2), bytes: 6 * kb);
  a('uploaded', 'Work/Roadmap.pptx', ago(days: 5), bytes: 4 * mb);
  a('uploaded', 'Documents/Resume.pdf', ago(days: 12), bytes: 212 * kb);
  a('uploaded', 'Documents/Lease agreement.docx', ago(days: 80), bytes: 96 * kb);
  rows.sort((x, y) => x.at.compareTo(y.at));
  for (final r in rows) {
    db.activity(r.kind, r.path, at: r.at, bytes: r.bytes, detail: r.detail);
  }
}

const AppConfig busyConfig = AppConfig(
  syncDir: r'C:\Users\Dhiva\CloudDrop',
  driveFolder: 'CloudDrop',
  clientId: '123456-clouddrop.apps.googleusercontent.com',
  hasClientSecret: true,
  parallelUploads: 2,
  settleSecs: 2,
  notifications: true,
  ignore: ['.clouddrop-*', '.goutputstream-*', '*.part', '*.crdownload', '*.tmp', '~\$*', '*.swp'],
  autoFreeDays: 30,
  keepLocal: ['Work', 'Photos/Goa trip'],
);

DaemonStatus uploadingDaemon() => DaemonStatus(
  pid: 4242,
  updatedMs: nowMs,
  phase: Phase.uploading,
  waiting: 3,
  uploads: const [
    Upload(path: 'Photos/IMG_20261012_184501.jpg', sent: 6400 * kb, total: 9800 * kb),
    Upload(path: 'backup-2026-10.zip', sent: 120 * mb, total: 410 * mb),
  ],
  quota: const Quota(limit: 15 * gb, usage: 11 * gb + 200 * mb, usageInDrive: 3 * gb),
);

DaemonStatus idleDaemon({Quota? quota}) => DaemonStatus(
  pid: 4242,
  updatedMs: nowMs,
  phase: Phase.idle,
  quota:
      quota ??
      const Quota(limit: 15 * gb, usage: 4 * gb + 700 * mb, usageInDrive: 2 * gb + 100 * mb),
);

/// The busy folder with uploads running.
TestApp busyApp({
  DaemonStatus? daemon,
  AppConfig config = busyConfig,
  bool activity = true,
  bool windows = true,
  bool cloudFiles = false,
  CloudProvider? provider,
}) {
  final t = TestApp(
    windows: windows,
    daemon: daemon ?? uploadingDaemon(),
    config: config,
    cloudFiles: cloudFiles,
    provider: provider,
  );
  fillBusyFolder(t.db);
  if (activity) fillActivity(t.db);
  return t;
}

/// A tidy folder where everything is uploaded.
TestApp calmApp({bool windows = true, CloudProvider? provider, DaemonStatus? daemon}) {
  final t = TestApp(
    windows: windows,
    daemon: daemon ?? idleDaemon(),
    config: busyConfig,
    provider: provider,
  );
  t.db.meta('root_id', 'ROOT-DRIVE-ID');
  t.db.dir('Documents');
  t.db.file(
    'Documents/Budget 2026.xlsx',
    size: 48 * kb,
    mtimeMs: ago(days: 2),
    uploadedAt: ago(days: 2),
  );
  t.db.file(
    'Documents/Resume.pdf',
    size: 212 * kb,
    mtimeMs: ago(days: 12),
    uploadedAt: ago(days: 12),
  );
  t.db.dir('Photos');
  for (var i = 1; i <= 6; i++) {
    t.db.file(
      'Photos/IMG_240$i.jpg',
      state: i > 3 ? 'cloud_only' : 'synced',
      size: 3 * mb,
      cloudSize: 3 * mb,
      mtimeMs: ago(days: 20 + i),
      uploadedAt: ago(days: 20 + i),
    );
  }
  t.db.file('Videos.zip', state: 'cloud_only', cloudSize: 2 * gb, uploadedAt: ago(days: 60));
  fillActivity(t.db);
  return t;
}

// ---------- several folders ----------

/// Two folders: the tidy CloudDrop folder (up to date, uploading to Google Drive) and "Work"
/// (uploading to OneDrive right now). [remember] is the one app.json names.
TestApp twoFoldersApp({String? remember}) {
  final t = calmApp();
  final work = t.addConnection(
    'Work',
    provider: oneDrive,
    driveFolder: 'Work',
    daemon: DaemonStatus(
      pid: 4243,
      updatedMs: nowMs,
      phase: Phase.uploading,
      waiting: 2,
      uploads: const [Upload(path: 'Plans/Roadmap 2027.pptx', sent: 3 * mb, total: 8 * mb)],
      quota: const Quota(limit: 1024 * gb, usage: 212 * gb, usageInDrive: 212 * gb),
    ),
  );
  work.db.meta('root_id', 'WORK-ROOT');
  work.db.dir('Plans');
  work.db.file(
    'Plans/Roadmap 2027.pptx',
    state: 'uploading',
    size: 8 * mb,
    mtimeMs: ago(minutes: 4),
  );
  work.db.file('Plans/Budget.xlsx', state: 'pending', size: 96 * kb, mtimeMs: ago(minutes: 3));
  work.db.file('Contracts.pdf', size: 1200 * kb, mtimeMs: ago(days: 4), uploadedAt: ago(days: 4));
  work.db.activity('uploaded', 'Contracts.pdf', at: ago(days: 4), bytes: 1200 * kb);
  t.prefs.remembered = remember;
  return t;
}
