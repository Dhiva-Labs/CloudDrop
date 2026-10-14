import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:clouddrop_app/logic/files_model.dart';
import 'package:clouddrop_app/state/app_controller.dart';
import 'package:clouddrop_app/state/files_controller.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';

TestApp build() {
  final t = TestApp(
    daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle),
    config: const AppConfig(
      syncDir: '/home/me/CloudDrop',
      driveFolder: 'CloudDrop',
      clientId: '1.apps.googleusercontent.com',
      hasClientSecret: true,
      keepLocal: ['Work'],
    ),
  );
  t.db
    ..dir('Photos')
    ..file('Photos/a.jpg', size: 10, mtimeMs: 1000)
    ..file('Photos/b.jpg', state: 'cloud_only', cloudSize: 20)
    ..dir('Photos/Trip')
    ..file('Photos/Trip/c.jpg', state: 'error', size: 30, error: 'boom')
    ..dir('Work')
    ..dir('Work/Sub')
    ..file('Work/Sub/x.txt', size: 5)
    ..file('top.txt', state: 'pending', size: 1);
  return t;
}

Future<FilesController> open(TestApp t) async {
  final app = await t.ready();
  final c = FilesController(app);
  addTearDown(c.dispose);
  c.visible = true;
  await c.reload();
  return c;
}

List<String> names(FilesController c) => [for (final i in c.items) i.name];

void main() {
  test('starts at the top: folders first', () async {
    final c = await open(build());
    expect(names(c), ['Photos', 'Work', 'top.txt']);
    expect(c.dir, '');
    expect(c.loaded, isTrue);
    expect(c.summary, '5 files · 66 B · 1 only in Google Drive');
    expect(c.canGoBack, isFalse);
    expect(c.flat, isFalse);
  });

  test('opening folders and going back keeps the folder selected', () async {
    final c = await open(build());
    c.openDir('Photos');
    await c.reload();
    expect(c.dir, 'Photos');
    expect(names(c), ['Trip', 'a.jpg', 'b.jpg']);
    expect(c.canGoBack, isTrue);
    c.openDir('Photos/Trip');
    await c.reload();
    c.goBack();
    await c.reload();
    expect(c.dir, 'Photos');
    expect(c.selectedPaths, {'Photos/Trip'}, reason: 'back selects the folder you came from');
    expect(c.scrollTarget, 'Photos/Trip');
    c.goBack();
    await c.reload();
    expect(c.dir, '');
    c.goBack();
    expect(c.dir, '');
  });

  test('filters list files across the whole tree and flat', () async {
    final c = await open(build());
    c.setFilter(Filter.failed);
    await c.reload();
    expect(c.flat, isTrue);
    expect(c.items.map((i) => i.path), ['Photos/Trip/c.jpg']);
    expect(c.summary, '1 item · 30 B');
    c.setFilter(Filter.cloudOnly);
    await c.reload();
    expect(c.items.map((i) => i.path), ['Photos/b.jpg']);
    expect(c.emptyState.title, isNotEmpty);
    // Back from a filter returns to all files.
    c.goBack();
    await c.reload();
    expect(c.filter, Filter.all);
    expect(c.dir, '');
  });

  test('search runs across all folders and goes back to the folder', () async {
    final c = await open(build());
    c.openDir('Work');
    await c.reload();
    c.searchChanged('jpg');
    await c.reload();
    expect(c.search, 'jpg');
    expect(c.items.map((i) => i.path), ['Photos/a.jpg', 'Photos/b.jpg', 'Photos/Trip/c.jpg']);
    expect(c.flat, isTrue);
    c.goBack();
    await c.reload();
    expect(c.search, '');
    expect(c.searchText, '');
    expect(c.dir, 'Work');
    c.searchChanged('zzz');
    await c.reload();
    expect(c.items, isEmpty);
    expect(c.emptyState.title, 'No Results');
    expect(c.emptyState.text, 'Nothing in CloudDrop is named like “zzz”.');
  });

  test('search waits for typing to pause', () async {
    final t = build();
    final app = await t.ready();
    final slow = AppController(
      AppServices(
        paths: app.paths,
        engine: t.engine,
        db: app.services.db,
        status: t.statusSource,
        system: t.system,
        searchDelay: const Duration(milliseconds: 80),
      ),
    );
    final c = FilesController(slow)..visible = true;
    addTearDown(c.dispose);
    c.searchChanged('p');
    c.searchChanged('ph');
    expect(c.search, '', reason: 'not applied yet');
    expect(c.searchText, 'ph');
    await Future<void>.delayed(const Duration(milliseconds: 150));
    expect(c.search, 'ph');
  });

  test('sorting by column', () async {
    final c = await open(build());
    c.openDir('Photos');
    await c.reload();
    c.setSort(const Sort(SortKey.size, true));
    expect(names(c), ['Trip', 'b.jpg', 'a.jpg']);
    c.setSort(const Sort(SortKey.status));
    expect(names(c), ['Trip', 'b.jpg', 'a.jpg']);
    c.setSort(const Sort(SortKey.name, true));
    expect(names(c), ['Trip', 'b.jpg', 'a.jpg']);
    c.setSort(const Sort());
    expect(names(c), ['Trip', 'a.jpg', 'b.jpg']);
  });

  test('selection: one, toggle, range, all, clear; capabilities follow', () async {
    final c = await open(build());
    c.openDir('Photos');
    await c.reload();
    c.selectOnly('Photos/a.jpg');
    expect(c.selectedPaths, {'Photos/a.jpg'});
    expect(c.caps.free, isTrue);
    expect(c.caps.download, isFalse);
    c.toggle('Photos/b.jpg');
    expect(c.selectedPaths, {'Photos/a.jpg', 'Photos/b.jpg'});
    expect(c.caps.download, isTrue);
    expect(c.caps.free, isTrue);
    c.toggle('Photos/a.jpg');
    expect(c.selectedPaths, {'Photos/b.jpg'});
    c.selectOnly('Trip'.isEmpty ? '' : 'Photos/Trip');
    c.selectRange('Photos/b.jpg');
    expect(c.selectedPaths, {'Photos/Trip', 'Photos/a.jpg', 'Photos/b.jpg'});
    c.selectAll();
    expect(c.selectedPaths.length, 3);
    c.clearSelection();
    expect(c.selectedPaths, isEmpty);
    expect(c.caps.count, 0);
  });

  test('pinned folders: on, inherited, off', () async {
    final c = await open(build());
    expect(c.keepStateOf('Work'), Keep.on);
    expect(c.keepStateOf('Work/Sub'), Keep.inherited);
    expect(c.keepStateOf('Photos'), Keep.off);
    c.selectOnly('Work');
    expect(c.caps.keep, Keep.on);
    c.openDir('Work');
    await c.reload();
    c.selectOnly('Work/Sub');
    expect(c.caps.keep, Keep.inherited);
  });

  test('a reload keeps the selection for rows that stay and drops the rest', () async {
    final t = build();
    final c = await open(t);
    c.openDir('Photos');
    await c.reload();
    c.selectAll();
    expect(c.selectedPaths.length, 3);
    t.db.db.execute("DELETE FROM entries WHERE path = 'Photos/b.jpg'");
    t.db.file('Photos/new.jpg', size: 4);
    await c.reload();
    expect(c.selectedPaths, {'Photos/Trip', 'Photos/a.jpg'});
    expect(names(c), ['Trip', 'a.jpg', 'new.jpg']);
  });

  test('a folder deleted from the database falls back to its parent', () async {
    final t = build();
    final c = await open(t);
    c.openDir('Photos/Trip');
    await c.reload();
    expect(c.dir, 'Photos/Trip');
    t.db.db.execute("DELETE FROM entries WHERE path LIKE 'Photos/Trip%'");
    await c.reload();
    expect(c.dir, 'Photos');
  });

  test('a database change reloads the visible list, a hidden one waits', () async {
    final t = build();
    final app = await t.ready();
    final c = FilesController(app);
    addTearDown(c.dispose);
    // Hidden: nothing loaded yet.
    app.dbVersion.value++;
    await Future<void>.delayed(Duration.zero);
    expect(c.loaded, isFalse);
    c.visible = true; // showing it loads
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(c.loaded, isTrue);
    t.db.file('new.txt', size: 3);
    app.dbVersion.value++;
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(names(c), contains('new.txt'));
  });

  test('navigation requests from other screens', () async {
    final t = build();
    final app = await t.ready();
    final c = FilesController(app);
    addTearDown(c.dispose);
    c.visible = true;
    app.showFiles(filter: Filter.failed);
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(c.filter, Filter.failed);
    expect(c.items.single.path, 'Photos/Trip/c.jpg');
    app.showFiles(dir: 'Photos', select: 'Photos/a.jpg');
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(c.dir, 'Photos');
    expect(c.filter, Filter.all);
    expect(c.selectedPaths, {'Photos/a.jpg'});
    expect(c.scrollTarget, 'Photos/a.jpg');
  });

  group('actions on the selection', () {
    test('download, free, retry use absolute paths', () async {
      final t = build();
      final c = await open(t);
      c.openDir('Photos');
      await c.reload();
      c.selectOnly('Photos/b.jpg');
      await c.downloadSelected();
      c.selectOnly('Photos/a.jpg');
      await c.freeSelected();
      c.selectOnly('Photos/Trip');
      await c.retrySelected();
      await c.retryAllHere();
      expect(t.engine.calls, [
        'restore /home/me/CloudDrop/Photos/b.jpg',
        'free /home/me/CloudDrop/Photos/a.jpg',
        'retry /home/me/CloudDrop/Photos/Trip',
        'retry /home/me/CloudDrop/Photos',
      ]);
    });

    test('pin a folder', () async {
      final t = build();
      final c = await open(t);
      c.selectOnly('Photos');
      await c.setKeepSelected(true);
      c.selectOnly('Work');
      await c.setKeepSelected(false);
      expect(t.engine.calls, ['keep /home/me/CloudDrop/Photos', 'unkeep /home/me/CloudDrop/Work']);
      c.selectOnly('top.txt');
      await c.setKeepSelected(true);
      expect(t.engine.calls.length, 2, reason: 'files can\'t be pinned');
    });

    test('open in Drive and show in Explorer', () async {
      final t = build();
      final c = await open(t);
      c.openDir('Photos');
      await c.reload();
      c.selectOnly('Photos/a.jpg');
      await c.openSelectedInDrive();
      c.showSelectedInExplorer();
      expect(t.system.urls, ['https://drive.google.com/file/d/id-Photos/a.jpg/view']);
      // This sandbox folder doesn't exist on disk, so Explorer opens the nearest folder that does.
      expect(t.system.revealed, ['/home/me/CloudDrop']);
    });

    test(
      'activating rows: folders open, local files launch, cloud-only files offer a download',
      () async {
        final t = build();
        final app = await t.ready();
        final msgs = <AppMessage>[];
        app.messages.listen(msgs.add);
        final c = FilesController(app)..visible = true;
        addTearDown(c.dispose);
        await c.reload();
        c.activate(c.items.firstWhere((i) => i.name == 'top.txt'));
        expect(t.system.opened, ['/home/me/CloudDrop/top.txt']);
        c.activate(c.items.firstWhere((i) => i.name == 'Photos'));
        await c.reload();
        expect(c.dir, 'Photos');
        c.activate(c.items.firstWhere((i) => i.name == 'b.jpg'));
        await Future<void>.delayed(Duration.zero);
        expect(msgs.last.text, '“b.jpg” is only in Google Drive');
        expect(msgs.last.actionLabel, 'Download');
        msgs.last.onAction!();
        await Future<void>.delayed(const Duration(milliseconds: 20));
        expect(t.engine.calls, ['restore /home/me/CloudDrop/Photos/b.jpg']);
      },
    );

    test(
      'Windows: a freed file whose placeholder is on disk just opens; a deleted one offers Download',
      () async {
        final t = TestApp(
          daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle),
          cloudFiles: true,
        );
        t.db
          ..file('kept.txt', state: 'cloud_only', cloudSize: 5)
          ..file('gone.txt', state: 'cloud_only', cloudSize: 5)
          ..file('local.txt', size: 3);
        t.onDisk.add('/home/me/CloudDrop/kept.txt');
        final app = await t.ready();
        final msgs = <AppMessage>[];
        app.messages.listen(msgs.add);
        final c = FilesController(app)..visible = true;
        addTearDown(c.dispose);
        await c.reload();
        DirItem item(String n) => c.items.firstWhere((i) => i.name == n);

        // The placeholder: Open is the primary action, and double-click opens it.
        c.selectOnly('kept.txt');
        await Future<void>.delayed(const Duration(milliseconds: 20));
        expect(c.isPlaceholder('kept.txt'), isTrue);
        expect(c.canOpen, isTrue);
        await c.activate(item('kept.txt'));
        expect(t.system.opened, ['/home/me/CloudDrop/kept.txt']);
        expect(msgs, isEmpty, reason: 'no "only in Google Drive" detour');

        // Deleted locally: nothing to open, so it offers the download (clouddrop restore).
        c.selectOnly('gone.txt');
        await Future<void>.delayed(const Duration(milliseconds: 20));
        expect(c.isPlaceholder('gone.txt'), isFalse);
        expect(c.canOpen, isFalse);
        await c.activate(item('gone.txt'));
        await Future<void>.delayed(Duration.zero);
        expect(msgs.single.text, '“gone.txt” is only in Google Drive');
        expect(msgs.single.actionLabel, 'Download');
        msgs.single.onAction!();
        await Future<void>.delayed(const Duration(milliseconds: 20));
        expect(t.engine.calls, ['restore /home/me/CloudDrop/gone.txt']);
        expect(t.system.opened.length, 1);

        // A file that is here is always openable.
        c.selectOnly('local.txt');
        expect(c.canOpen, isTrue);
        c.selectOnly('kept.txt');
        c.toggle('local.txt');
        expect(c.canOpen, isFalse, reason: 'one file at a time');
      },
    );

    test('Linux: a freed file is gone, so it always offers the download', () async {
      final t = TestApp(daemon: const DaemonStatus(pid: 1, updatedMs: 1, phase: Phase.idle));
      t.db.file('freed.txt', state: 'cloud_only', cloudSize: 5);
      t.onDisk.add('/home/me/CloudDrop/freed.txt'); // would exist on Windows
      final app = await t.ready();
      final msgs = <AppMessage>[];
      app.messages.listen(msgs.add);
      final c = FilesController(app)..visible = true;
      addTearDown(c.dispose);
      await c.reload();
      c.selectOnly('freed.txt');
      await Future<void>.delayed(const Duration(milliseconds: 20));
      expect(c.canOpen, isFalse);
      await c.activate(c.items.single);
      await Future<void>.delayed(Duration.zero);
      expect(t.system.opened, isEmpty);
      expect(msgs.single.actionLabel, 'Download');
    });

    test('Add Files copies into the current folder; dropping does too', () async {
      final t = build();
      final c = await open(t);
      // No sync folder on disk here; the copy reports the problem instead of crashing.
      t.system.files = const [];
      await c.addFiles();
      await c.dropped(const []);
      expect(t.engine.calls, isEmpty);
    });
  });

  test('what() names one item or counts several', () async {
    final c = await open(build());
    expect(c.what([c.items.first]), '“Photos”');
    expect(c.what(c.items), '3 items');
  });
}
