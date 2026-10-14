/// The sample folder behind the Microsoft Store screenshots: a believable CloudDrop of a person
/// who backs up photos, documents, projects and videos, in the second week of October 2026.
///
/// Everything is made up, but the numbers add up: the Drive card, the "freed" card and the
/// file lists all come from the same rows.
library;

import 'dart:math';

import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:sqlite3/sqlite3.dart';

import '../support/fakes.dart';
import '../support/harness.dart';

const int _kb = 1024;
const int _mb = 1024 * 1024;
const int _gb = 1024 * 1024 * 1024;

const String storeUser = 'Alex';
const String storeSyncDir = r'C:\Users\Alex\CloudDrop';

/// Settings of the sample account: three uploads at a time, automatic freeing after 30 days,
/// Documents always kept on this computer.
const AppConfig storeConfig = AppConfig(
  syncDir: storeSyncDir,
  driveFolder: 'CloudDrop',
  clientId: '480123456789-clouddrop.apps.googleusercontent.com',
  hasClientSecret: true,
  parallelUploads: 3,
  settleSecs: 2,
  notifications: true,
  ignore: ['.clouddrop-*', '*.part', '*.crdownload', '*.tmp', '~\$*', 'Thumbs.db', 'desktop.ini'],
  autoFreeDays: 30,
  keepLocal: ['Documents'],
);

/// Inserts rows the way the engine records them (fast: one prepared statement).
class _Seeder {
  _Seeder(this.db)
    : _insert = db.prepare(
        'INSERT OR REPLACE INTO entries (path, kind, state, drive_id, md5, cloud_size, local_size, '
        'local_mtime_ns, updated_at, uploaded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)',
      );

  final Database db;
  final PreparedStatement _insert;
  final Random random = Random(2026);

  void dir(String path, {int? mtimeMs}) {
    _insert.execute([
      path,
      'dir',
      'synced',
      'dir-$path',
      null,
      null,
      null,
      mtimeMs == null ? null : mtimeMs * 1000000,
      null,
    ]);
  }

  /// [state]: synced | cloud_only | uploading | pending.
  void file(String path, {required int size, required int mtimeMs, String state = 'synced'}) {
    final uploaded = state == 'synced' || state == 'cloud_only';
    _insert.execute([
      path,
      'file',
      state,
      state == 'pending' ? null : 'file-$path',
      uploaded ? 'md5-$path' : null,
      uploaded ? size : null,
      state == 'cloud_only' ? null : size,
      state == 'cloud_only' ? null : mtimeMs * 1000000,
      uploaded ? mtimeMs + 60000 : null,
    ]);
  }

  int between(int lo, int hi) => lo + random.nextInt(hi - lo + 1);

  /// [count] photos `IMG_<n>.jpg` in [dir], taken between [from] and [to] (unix ms), 3-9 MB.
  void photos(
    String dir, {
    required int count,
    required int firstNo,
    required int from,
    required int to,
    double cloudOnly = 0,
    int minMb = 3,
    int maxMb = 9,
  }) {
    for (var i = 0; i < count; i++) {
      final at = from + ((to - from) * i ~/ max(1, count - 1));
      final cloud = i < count * cloudOnly;
      file(
        '$dir/IMG_${firstNo + i}.jpg',
        size: between(minMb * 100, maxMb * 100) * _mb ~/ 100,
        mtimeMs: at,
        state: cloud ? 'cloud_only' : 'synced',
      );
    }
  }

  /// [count] documents named `<prefix> <n>.<ext>`.
  void documents(
    String dir,
    String prefix,
    String ext, {
    required int count,
    required int from,
    required int to,
    int minKb = 40,
    int maxKb = 900,
    String state = 'synced',
  }) {
    for (var i = 0; i < count; i++) {
      final at = from + ((to - from) * i ~/ max(1, count - 1));
      file(
        '$dir/$prefix ${(i + 1).toString().padLeft(3, '0')}.$ext',
        size: between(minKb, maxKb) * _kb,
        mtimeMs: at,
        state: state,
      );
    }
  }
}

int _days(int d, {int hours = 0, int minutes = 0}) => ago(days: d, hours: hours, minutes: minutes);

/// The sample CloudDrop on Windows: uploading a few photos right now, nothing failed.
TestApp storeApp() {
  final t = TestApp(windows: true, windowsUser: storeUser, config: storeConfig, daemon: null);
  final db = t.db.db;
  t.db.meta('root_id', '1CloudDropRootFolderId');
  db.execute('BEGIN');
  final s = _Seeder(db);

  // ---------- Photos ----------
  s.dir('Photos', mtimeMs: ago(minutes: 4));
  s.dir('Photos/2026', mtimeMs: ago(minutes: 4));
  s.dir('Photos/2026/Cycling Sunday', mtimeMs: _days(3));
  s.dir('Photos/2026/Goa trip', mtimeMs: _days(17));
  s.dir('Photos/2026/Pune weekend', mtimeMs: _days(3, hours: 4));
  s.photos('Photos/2026/Goa trip', count: 96, firstNo: 4521, from: _days(20), to: _days(16));
  s.photos('Photos/2026/Pune weekend', count: 41, firstNo: 4871, from: _days(4), to: _days(3));
  s.photos(
    'Photos/2026/Cycling Sunday',
    count: 38,
    firstNo: 4922,
    from: _days(3, hours: 6),
    to: _days(3, hours: 3),
  );
  // Loose photos of the last weeks: the old ones are already freed, the new ones just landed.
  s.photos('Photos/2026', count: 12, firstNo: 4410, from: _days(48), to: _days(36), cloudOnly: 1);
  s.photos('Photos/2026', count: 9, firstNo: 4980, from: _days(9), to: _days(7));
  s.photos('Photos/2026', count: 8, firstNo: 5060, from: _days(5), to: _days(1, hours: 2));
  s.photos(
    'Photos/2026',
    count: 10,
    firstNo: 5111,
    from: ago(hours: 2, minutes: 40),
    to: ago(minutes: 22),
  );
  // On their way up right now.
  for (final (no, state, minutes, mb100) in const [
    (5121, 'uploading', 9, 720),
    (5122, 'uploading', 8, 640),
    (5123, 'pending', 6, 580),
    (5124, 'pending', 5, 810),
    (5125, 'pending', 4, 690),
  ]) {
    s.file(
      'Photos/2026/IMG_$no.jpg',
      size: mb100 * _mb ~/ 100,
      mtimeMs: ago(minutes: minutes),
      state: state,
    );
  }
  s.dir('Photos/2025', mtimeMs: _days(120));
  s.photos(
    'Photos/2025',
    count: 410,
    firstNo: 1010,
    from: _days(640),
    to: _days(300),
    cloudOnly: 1,
  );
  s.dir('Photos/2024', mtimeMs: _days(400));
  s.photos(
    'Photos/2024',
    count: 360,
    firstNo: 1010,
    from: _days(1000),
    to: _days(700),
    cloudOnly: 1,
  );
  s.dir('Photos/2023', mtimeMs: _days(700));
  s.photos(
    'Photos/2023',
    count: 280,
    firstNo: 1010,
    from: _days(1400),
    to: _days(1100),
    cloudOnly: 1,
  );

  // ---------- Documents (always kept on this computer) ----------
  s.dir('Documents', mtimeMs: _days(1, hours: 3));
  for (final (name, count) in const [
    ('Invoices', 48),
    ('Tax', 22),
    ('Travel', 31),
    ('Health', 14),
    ('Education', 27),
  ]) {
    s.dir('Documents/$name', mtimeMs: _days(6));
    s.documents(
      'Documents/$name',
      name == 'Invoices' ? 'Invoice 2026-' : name,
      'pdf',
      count: count,
      from: _days(200),
      to: _days(2),
    );
  }
  s.documents(
    'Documents',
    'Letter',
    'docx',
    count: 12,
    from: _days(90),
    to: _days(5),
    minKb: 30,
    maxKb: 400,
  );

  // ---------- Projects ----------
  s.dir('Projects', mtimeMs: _days(1, hours: 5));
  s.dir('Projects/Website redesign', mtimeMs: _days(1, hours: 5));
  s.documents(
    'Projects/Website redesign',
    'Mockup',
    'png',
    count: 62,
    from: _days(25),
    to: _days(1),
    minKb: 300,
    maxKb: 4200,
  );
  s.dir('Projects/Mobile app', mtimeMs: _days(4));
  s.documents(
    'Projects/Mobile app',
    'Build notes',
    'md',
    count: 80,
    from: _days(28),
    to: _days(4),
    minKb: 20,
    maxKb: 700,
  );
  s.documents(
    'Projects/Mobile app',
    'Release',
    'zip',
    count: 38,
    from: _days(150),
    to: _days(40),
    minKb: 4000,
    maxKb: 24000,
    state: 'cloud_only',
  );
  s.dir('Projects/Client work', mtimeMs: _days(45));
  s.documents(
    'Projects/Client work',
    'Deliverable',
    'zip',
    count: 74,
    from: _days(380),
    to: _days(46),
    minKb: 2000,
    maxKb: 18000,
    state: 'cloud_only',
  );
  s.dir('Projects/Archive', mtimeMs: _days(300));
  s.documents(
    'Projects/Archive',
    'Project',
    'zip',
    count: 210,
    from: _days(1200),
    to: _days(400),
    minKb: 800,
    maxKb: 9000,
    state: 'cloud_only',
  );

  // ---------- Videos ----------
  s.dir('Videos', mtimeMs: _days(3, hours: 4));
  for (final (name, gb100, daysAgo, local) in const [
    ('Pune weekend drone.mp4', 190, 3, true),
    ('Cycling Sunday POV.mp4', 240, 3, true),
    ('Goa sunset timelapse.mp4', 120, 17, true),
    ('Family reunion 2025.mp4', 480, 330, false),
    ('Wedding highlights.mp4', 610, 250, false),
    ('Kerala backwaters.mp4', 360, 210, false),
  ]) {
    s.file(
      'Videos/$name',
      size: gb100 * _gb ~/ 100,
      mtimeMs: _days(daysAgo),
      state: local ? 'synced' : 'cloud_only',
    );
  }
  for (var i = 0; i < 25; i++) {
    s.file(
      'Videos/Clip ${(i + 1).toString().padLeft(2, '0')} - ${const ['Birthday', 'Trek', 'Beach', 'Market', 'Concert'][i % 5]}.mp4',
      size: s.between(180, 640) * _mb,
      mtimeMs: _days(s.between(60, 900)),
      state: 'cloud_only',
    );
  }

  // ---------- loose files in the top folder ----------
  s.file('Budget 2026.xlsx', size: 86 * _kb, mtimeMs: _days(3, hours: 1));
  s.file('Lease agreement 2025.pdf', size: 1260 * _kb, mtimeMs: _days(280), state: 'cloud_only');
  s.file('Passport scan.pdf', size: 1840 * _kb, mtimeMs: _days(16));
  s.file('Podcast - Ep 12 raw.wav', size: 612 * _mb, mtimeMs: _days(66), state: 'cloud_only');
  s.file('Q3 planning deck.pptx', size: 14540 * _kb, mtimeMs: _days(6, hours: 2));
  s.file('Scan 0142.jpg', size: 4300 * _kb, mtimeMs: ago(minutes: 11), state: 'uploading');
  s.file('Wedding invitation.pdf', size: 3480 * _kb, mtimeMs: _days(130), state: 'cloud_only');
  db.execute('COMMIT');

  // ---------- make the totals what the screenshots say ----------
  // 18.4 GB freed on this computer: adjust one old video.
  final freedTarget = (18.4 * _gb).round();
  final freed =
      db.select("SELECT sum(cloud_size) AS n FROM entries WHERE state = 'cloud_only'").first['n']
          as int;
  final reunion =
      db
              .select(
                "SELECT cloud_size AS n FROM entries WHERE path = 'Videos/Family reunion 2025.mp4'",
              )
              .first['n']
          as int;
  db.execute("UPDATE entries SET cloud_size = ?1 WHERE path = 'Videos/Family reunion 2025.mp4'", [
    reunion + freedTarget - freed,
  ]);
  final held =
      db
              .select(
                "SELECT sum(cloud_size) AS n FROM entries WHERE kind = 'file' AND md5 IS NOT NULL",
              )
              .first['n']
          as int;

  fillActivityForStore(t);

  // The engine's view: three uploads running, three more waiting, 46.2 GB of 100 GB used.
  t.statusSource.value = DaemonStatus(
    pid: 5120,
    updatedMs: nowMs,
    phase: Phase.uploading,
    waiting: 6,
    uploads: const [
      Upload(path: 'Photos/2026/IMG_5121.jpg', sent: 5200 * _kb, total: 7372 * _kb),
      Upload(path: 'Photos/2026/IMG_5122.jpg', sent: 2480 * _kb, total: 6554 * _kb),
      Upload(path: 'Scan 0142.jpg', sent: 1310 * _kb, total: 4300 * _kb),
    ],
    quota: Quota(limit: 100 * _gb, usage: (46.2 * _gb).round(), usageInDrive: held + 400 * _mb),
  );
  return t;
}

/// Activity of today and yesterday: uploads, freed space, moves, downloads.
void fillActivityForStore(TestApp t) {
  final rows = <({String kind, String path, int at, int? bytes, String? detail})>[];
  void a(String kind, String path, int at, {int? bytes, String? detail}) =>
      rows.add((kind: kind, path: path, at: at, bytes: bytes, detail: detail));

  // today
  a('uploaded', 'Photos/2026/IMG_5120.jpg', ago(minutes: 9), bytes: 5815 * _kb);
  a('uploaded', 'Photos/2026/IMG_5119.jpg', ago(minutes: 14), bytes: 6710 * _kb);
  a('uploaded', 'Photos/2026/IMG_5118.jpg', ago(minutes: 21), bytes: 4980 * _kb);
  a('auto_freed', 'Photos/2024/IMG_1422.jpg', ago(hours: 1, minutes: 12), bytes: 5130 * _kb);
  a('freed', 'Videos/Family reunion 2025.mp4', ago(hours: 2, minutes: 5), bytes: 4800 * _mb);
  a(
    'updated',
    'Projects/Website redesign/Mockup 014.png',
    ago(hours: 3, minutes: 35),
    bytes: 2260 * _kb,
  );
  a('restored', 'Documents/Travel/Travel 007.pdf', ago(hours: 4, minutes: 48), bytes: 640 * _kb);
  a(
    'moved',
    'Documents/Invoices/Invoice 2026-047.pdf',
    ago(hours: 5, minutes: 20),
    detail: 'Documents/Invoice 2026-047.pdf',
    bytes: 312 * _kb,
  );
  // yesterday
  a('uploaded', 'Videos/Cycling Sunday POV.mp4', ago(hours: 20, minutes: 40), bytes: 2457 * _mb);
  a(
    'uploaded',
    'Photos/2026/Cycling Sunday/IMG_4944.jpg',
    ago(hours: 21, minutes: 10),
    bytes: 7120 * _kb,
  );
  a(
    'auto_freed',
    'Projects/Client work/Deliverable 061.zip',
    ago(hours: 22, minutes: 30),
    bytes: 9440 * _kb,
  );
  a(
    'moved',
    'Documents/Letter 004.docx',
    ago(hours: 24, minutes: 15),
    detail: 'Documents/Cover letter.docx',
    bytes: 96 * _kb,
  );
  a('restored', 'Photos/2025/IMG_1288.jpg', ago(hours: 26, minutes: 5), bytes: 5560 * _kb);
  a('uploaded', 'Budget 2026.xlsx', ago(hours: 27, minutes: 50), bytes: 86 * _kb);
  rows.sort((x, y) => x.at.compareTo(y.at));
  for (final r in rows) {
    t.db.activity(r.kind, r.path, at: r.at, bytes: r.bytes, detail: r.detail);
  }
}
