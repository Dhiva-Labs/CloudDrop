"""Several clouds at once: every connection's folder, database and provider."""

import os
import time
import unittest
from unittest import mock

import support
from support import ExtensionTestCase, FakeFile, Writer, ext


def names(items):
    return [item.name for item in items]


def strings(items):
    """Every label and tip of a menu, submenus included, in order."""
    found = []
    for item in items:
        found += [item.label, item.tip]
        if item.submenu:
            found += strings(item.submenu.items)
    return found


def by_name(items, name):
    (item,) = [i for i in items if i.name == name]
    return item


class Folder:
    """One connection as the CLI and the daemon leave it: config.toml, a sync
    folder and a state database, at the paths the extension reads."""

    def __init__(self, test, name, root):
        self.test = test
        self.name = name
        self.root = root
        if name == 'default':
            self.config = os.environ['CLOUDDROP_CONFIG']
            self.data = test.data
        else:
            self.config = os.path.join(
                os.path.dirname(os.environ['CLOUDDROP_CONFIG']),
                'connections', name, 'config.toml')
            self.data = os.path.join(test.data, 'connections', name)
        self.db_file = os.path.join(self.data, 'state.db')
        self.writer = None
        os.makedirs(root, exist_ok=True)

    def write_config(self, provider=None, keep=()):
        lines = ['sync_dir = "%s"' % self.root]
        if provider:
            lines.append('provider = "%s"' % provider)
        if keep:
            lines.append('keep_local = [%s]' % ', '.join(
                '"%s"' % k for k in keep))
        os.makedirs(os.path.dirname(self.config), exist_ok=True)
        with open(self.config, 'w') as f:
            f.write('\n'.join(lines) + '\n')
        os.utime(self.config, ns=(time.time_ns(), time.time_ns()))

    def remove_config(self):
        os.remove(self.config)

    def make_db(self, entries=(), root_id='ROOT'):
        """entries are (path, state[, kind[, drive_id[, error]]])."""
        os.makedirs(self.data, exist_ok=True)
        self.writer = Writer(self.db_file)
        self.test.addCleanup(self.writer.close)
        self.writer.set_meta('schema_version', '3')
        self.writer.set_meta('sync_root', self.root)
        if root_id:
            self.writer.set_meta('root_id', root_id)
        for entry in entries:
            self.put(*entry)

    def put(self, path, state='synced', kind='file', drive_id=None,
            error=None):
        self.writer.put(path, state, kind, drive_id, error)

    def abspath(self, rel):
        return os.path.join(self.root, rel) if rel else self.root

    def file(self, rel, is_dir=False):
        return FakeFile(self.abspath(rel), is_dir=is_dir)


class ConnectionsTestCase(ExtensionTestCase):
    """config.toml in <tmp>/config, so the named connections are in
    <tmp>/config/connections/ and their data in <data>/connections/."""

    def setUp(self):
        super().setUp()
        os.environ['CLOUDDROP_CONFIG'] = os.path.join(
            self.tmp, 'config', 'config.toml')

    def folder(self, name, folder, provider=None, entries=(), keep=(),
               config=True, db=True, root_id='ROOT'):
        made = Folder(self, name, os.path.join(self.tmp, folder))
        if config:
            made.write_config(provider, keep)
        if db:
            made.make_db(entries, root_id)
        return made

    def annotate_in(self, extension, folder, rel, is_dir=False):
        file = folder.file(rel, is_dir)
        extension.update_file_info(file)
        return file


class ThreeClouds(ConnectionsTestCase):
    """~/CloudDrop to Google Drive, ~/Work to Nextcloud, ~/Work2 to S3."""

    def setUp(self):
        super().setUp()
        self.home = self.folder('default', 'CloudDrop', entries=[
            ('a.txt', 'synced', 'file', 'ID-A'),
            ('d', 'synced', 'dir', 'ID-D'),
            ('d/x.txt', 'synced'),
        ])
        self.work = self.folder('work', 'Work', 'nextcloud', keep=['docs'],
                                entries=[
            ('a.txt', 'pending'),
            ('b.txt', 'synced', 'file', '/CloudDrop/b.txt'),
            ('docs', 'synced', 'dir', '/CloudDrop/docs'),
            ('gone.txt', 'cloud_only'),
        ])
        # A name with a space, and a folder whose name starts like Work's.
        self.photos = self.folder('My Photos', 'Work2', 's3', entries=[
            ('a.txt', 'error', 'file', None, 'access denied'),
            ('b.txt', 'synced', 'file', 'CloudDrop/b.txt'),
            ('pic.jpg', 'cloud_only'),
        ])
        self.extension = self.new_extension()

    def items(self, folder, *rels):
        files = [folder.file(rel, is_dir=rel in ('', 'd', 'docs'))
                 for rel in rels]
        return self.extension.get_file_items(files)

    def background(self, folder, rel=''):
        return self.extension.get_background_items(folder.file(rel, True))

    def argv(self, item):
        with mock.patch.object(ext, 'launch') as launch:
            item.activate()
        (args,), _kwargs = launch.call_args
        return list(args)


class EmblemTests(ThreeClouds):

    def test_each_file_uses_its_folders_database(self):
        cases = [
            (self.home, [ext.EMBLEM_SYNCED], 'Uploaded'),
            (self.work, [ext.EMBLEM_BUSY], 'Waiting to upload'),
            (self.photos, [ext.EMBLEM_ERROR], 'Upload failed: access denied'),
        ]
        for folder, emblems, label in cases:
            with self.subTest(folder.name):
                file = self.annotate_in(self.extension, folder, 'a.txt')
                self.assertEqual(file.emblems, emblems)
                self.assertEqual(file.label, label)   # the column's value
                tracked = self.extension._seen[folder.abspath('a.txt')]
                self.assertEqual(tracked.connection, folder.name)

    def test_directories_aggregate_their_own_database(self):
        self.work.put('docs/deep/x', 'error')
        home = self.annotate_in(self.extension, self.home, 'd', True)
        docs = self.annotate_in(self.extension, self.work, 'docs', True)
        self.assertEqual((home.emblems, home.label),
                         ([ext.EMBLEM_SYNCED], 'Uploaded'))
        self.assertEqual((docs.emblems, docs.label),
                         ([ext.EMBLEM_ERROR], 'Upload failed'))

    def test_files_outside_every_folder_are_ignored(self):
        paths = [
            os.path.join(self.tmp, 'a.txt'),
            os.path.join(self.tmp, 'Wor', 'a.txt'),
            os.path.join(self.tmp, 'Work-old', 'a.txt'),
            os.path.join(self.tmp, 'Work3', 'a.txt'),
            os.path.join(self.tmp, 'CloudDrop2', 'a.txt'),
            self.work.config,
            self.work.db_file,
            self.tmp,
            self.home.root, self.work.root, self.photos.root,   # the folders
        ]
        roots = (self.home.root, self.work.root, self.photos.root)
        for path in paths:
            with self.subTest(path):
                file = FakeFile(path, is_dir=True)
                self.extension.update_file_info(file)
                self.assertEqual((file.emblems, file.attributes), ([], {}))
                if path not in roots:   # a folder itself has menus
                    self.assertEqual(self.extension.get_file_items(
                        [FakeFile(path)]), [])
                    self.assertEqual(
                        self.extension.get_background_items(file), [])
        self.assertEqual(len(self.extension._seen), 0)

    def test_nested_looking_names_are_not_confused(self):
        # ~/Work2 is not inside ~/Work: its a.txt is My Photos', not work's.
        file = self.annotate_in(self.extension, self.photos, 'a.txt')
        self.assertEqual(file.label, 'Upload failed: access denied')
        self.assertEqual(
            self.extension._seen[self.photos.abspath('a.txt')].connection,
            'My Photos')
        (free,) = self.items(self.photos, 'b.txt')
        self.assertEqual(self.argv(free)[:3],
                         [self.binary, '--connection', 'My Photos'])

    def test_a_folder_named_like_another_without_a_connection(self):
        self.photos.remove_config()
        self.extension.poll()
        file = self.annotate_in(self.extension, self.photos, 'a.txt')
        self.assertEqual((file.emblems, file.attributes), ([], {}))
        self.assertEqual(self.items(self.photos, 'b.txt'), [])

    def test_only_the_changed_database_refreshes(self):
        files = {folder.name: self.annotate_in(self.extension, folder,
                                               'a.txt')
                 for folder in (self.home, self.work, self.photos)}
        self.extension.poll()   # baseline
        self.work.put('a.txt', 'synced')
        self.extension.poll()
        self.assertEqual({n: f.invalidated for n, f in files.items()},
                         {'default': 0, 'work': 1, 'My Photos': 0})
        self.assertEqual(
            self.extension._seen[self.work.abspath('a.txt')].status,
            ext.UPLOADED)

    def test_a_busy_database_holds_back_only_its_own_files(self):
        home = self.annotate_in(self.extension, self.home, 'a.txt')
        work = self.annotate_in(self.extension, self.work, 'a.txt')
        self.extension.poll()   # baseline
        self.home.put('a.txt', 'pending')
        self.work.put('a.txt', 'synced')
        (work_db,) = [c.db for c in self.extension._connections
                      if c.name == 'work']
        with mock.patch.object(work_db, 'data_version',
                               side_effect=ext.Unavailable('busy')):
            self.extension.poll()
        self.assertEqual((home.invalidated, work.invalidated), (1, 0))
        self.extension.poll()   # work's turn once it can be read
        self.assertEqual((home.invalidated, work.invalidated), (1, 1))

    def test_overlapping_folders_go_to_the_first_connection(self):
        # The CLI refuses this; the daemon skips the later connection, so the
        # default one (first) is the one uploading the file.
        inner = self.folder('inner', os.path.join('CloudDrop', 'd'),
                            'dropbox', entries=[('x.txt', 'error')])
        self.extension.poll()
        file = self.annotate_in(self.extension, inner, 'x.txt')
        self.assertEqual(file.label, 'Uploaded')
        self.assertEqual(self.extension._seen[inner.abspath('x.txt')]
                         .connection, 'default')


class MenuTests(ThreeClouds):

    def test_file_menus_name_each_connections_provider(self):
        self.assertEqual(strings(self.items(self.home, 'a.txt')), [
            'Free Up Space',
            'Remove local copies that are safely stored in Google Drive',
            'Open in Google Drive', 'Open this item in Google Drive'])
        self.assertEqual(strings(self.items(self.work, 'b.txt')), [
            'Free Up Space',
            'Remove local copies that are safely stored in Nextcloud',
            'Open in Nextcloud', 'Open this item in Nextcloud'])
        # S3 items have no web page: no "Open in".
        self.assertEqual(strings(self.items(self.photos, 'b.txt')), [
            'Free Up Space',
            'Remove local copies that are safely stored in S3'])

    def test_file_commands_name_their_connection(self):
        home, work, photos = self.home, self.work, self.photos
        b = self.binary
        cases = [
            (self.items(home, 'a.txt'), 'CloudDrop::free',
             [b, '--connection', 'default', '--notify', 'free',
              home.abspath('a.txt')]),
            (self.items(home, 'a.txt'), 'CloudDrop::open',
             [b, '--connection', 'default', 'open', home.abspath('a.txt')]),
            (self.items(work, 'b.txt', 'a.txt', 'docs'), 'CloudDrop::free',
             [b, '--connection', 'work', '--notify', 'free',
              work.abspath('b.txt'), work.abspath('docs')]),
            (self.items(work, 'b.txt'), 'CloudDrop::open',
             [b, '--connection', 'work', 'open', work.abspath('b.txt')]),
            (self.items(photos, 'a.txt'), 'CloudDrop::retry',
             [b, '--connection', 'My Photos', '--notify', 'retry',
              photos.abspath('a.txt')]),
            (self.items(work, ''), 'CloudDrop::open',
             [b, '--connection', 'work', 'open', work.root]),
        ]
        for items, name, argv in cases:
            with self.subTest(argv):
                self.assertEqual(self.argv(by_name(items, name)), argv)

    def test_keep_follows_each_connections_config(self):
        # work's config.toml keeps "docs"; the default one keeps nothing.
        self.home.put('docs', 'synced', 'dir')
        os.makedirs(self.home.abspath('docs'))
        work = self.items(self.work, 'docs')
        home = self.items(self.home, 'docs')
        self.assertEqual(names(work)[-1], 'CloudDrop::unkeep')
        self.assertEqual(names(home)[-1], 'CloudDrop::keep')
        self.assertEqual(self.argv(work[-1]), [
            self.binary, '--connection', 'work', '--notify', 'unkeep',
            self.work.abspath('docs')])
        self.assertEqual(self.argv(home[-1]), [
            self.binary, '--connection', 'default', '--notify', 'keep',
            self.home.abspath('docs')])

    def test_background_menus(self):
        work = self.background(self.work)
        self.assertEqual(strings(work), [
            'Cloud-only Files (1)',
            'Files kept in Nextcloud but removed from this computer',
            'Download All', 'Download every cloud-only item in this folder',
            'gone.txt', 'Download gone.txt from Nextcloud',
            'Open Folder in Nextcloud', 'Open this folder in Nextcloud'])
        self.assertEqual(self.argv(work[1]), [
            self.binary, '--connection', 'work', 'open', self.work.root])
        photos = self.background(self.photos)
        self.assertEqual(names(photos), ['CloudDrop::cloud_only'])
        self.assertEqual(
            [self.argv(item) for item in photos[0].submenu.items],
            [[self.binary, '--connection', 'My Photos', '--notify', 'restore',
              self.photos.root],
             [self.binary, '--connection', 'My Photos', '--notify', 'restore',
              self.photos.abspath('pic.jpg')]])
        self.assertEqual(names(self.background(self.home)),
                         ['CloudDrop::open_folder'])

    def test_a_selection_across_connections_has_no_menu(self):
        files = [self.home.file('a.txt'), self.work.file('b.txt')]
        self.assertEqual(self.extension.get_file_items(files), [])
        self.assertEqual(self.extension.get_file_items(files[::-1]), [])


class ColumnTests(ConnectionsTestCase):

    def column(self):
        (column,) = self.new_extension().get_columns()
        self.assertEqual((column.name, column.attribute),
                         ('CloudDrop::status', 'clouddrop_status'))
        return column.label, column.description

    def test_one_provider_everywhere_names_the_column(self):
        self.folder('default', 'CloudDrop')
        self.folder('work', 'Work', 'gdrive')
        self.assertEqual(self.column(),
                         ('Google Drive', 'Upload status in Google Drive'))

    def test_one_other_provider_everywhere(self):
        self.folder('default', 'CloudDrop', 'nextcloud')
        self.folder('work', 'Work', 'nextcloud')
        self.folder('Home Server', 'Home', 'nextcloud', db=False)
        self.assertEqual(self.column(),
                         ('Nextcloud', 'Upload status in Nextcloud'))

    def test_different_providers_make_a_neutral_column(self):
        self.folder('default', 'CloudDrop')
        self.folder('work', 'Work', 'nextcloud')
        self.assertEqual(self.column(), (
            'CloudDrop',
            'Upload status in the cloud of each CloudDrop folder'))

    def test_only_named_connections(self):
        # No default connection at all (or only its database): the named ones
        # decide.
        self.folder('work', 'Work', 'onedrive')
        self.assertEqual(self.column()[0], 'OneDrive')
        self.folder('default', 'CloudDrop', config=False)
        self.assertEqual(self.column()[0], 'OneDrive')

    def test_unknown_provider_is_another_cloud(self):
        self.folder('default', 'CloudDrop', 'floppy')
        self.folder('work', 'Work', 'gdrive')
        self.assertEqual(self.column()[0], 'CloudDrop')

    def test_column_title_rule(self):
        p = ext.PROVIDERS
        self.assertEqual(ext.column_title([])[0], 'Google Drive')
        self.assertEqual(ext.column_title([p['mega']])[0], 'MEGA')
        self.assertEqual(ext.column_title([p['s3'], p['s3']])[0], 'S3')
        self.assertEqual(ext.column_title([p['s3'], p['sftp']])[0],
                         'CloudDrop')


class AddedAndRemovedTests(ConnectionsTestCase):

    def setUp(self):
        super().setUp()
        self.home = self.folder('default', 'CloudDrop',
                                entries=[('a.txt', 'synced')])
        self.extension = self.new_extension()
        self.work_root = os.path.join(self.tmp, 'Work')

    def add_work(self, config=True, db=True):
        return self.folder('work', 'Work', 'nextcloud', config=config, db=db,
                           entries=[('a.txt', 'pending'),
                                    ('b.txt', 'synced', 'file', '/b.txt')])

    def test_connection_added_while_running(self):
        shown = FakeFile(os.path.join(self.work_root, 'a.txt'))
        self.extension.update_file_info(shown)   # before it is a connection
        self.assertEqual(shown.attributes, {})
        self.add_work()
        # Noticed on the tick, not on every file.
        later = FakeFile(os.path.join(self.work_root, 'b.txt'))
        self.extension.update_file_info(later)
        self.assertEqual(later.attributes, {})
        self.extension.poll()
        # The items already shown are asked about again ...
        self.assertEqual((shown.invalidated, later.invalidated), (1, 1))
        self.extension.update_file_info(shown)
        self.assertEqual(shown.label, 'Waiting to upload')
        # ... and menus name the new connection.
        free = self.extension.get_file_items(
            [FakeFile(os.path.join(self.work_root, 'b.txt'))])[0]
        with mock.patch.object(ext, 'launch') as launch:
            free.activate()
        self.assertEqual(launch.call_args.args[0][:3],
                         (self.binary, '--connection', 'work'))
        self.extension.poll()
        self.assertEqual(shown.invalidated, 1)

    def test_connection_set_up_before_its_database_exists(self):
        work = self.add_work(db=False)
        self.extension.poll()
        file = self.annotate_in(self.extension, work, 'a.txt')
        self.assertEqual(file.attributes, {})   # unknown for now
        self.assertEqual(self.extension.get_file_items([file]), [])
        self.extension.poll()
        self.assertEqual(file.invalidated, 0)
        work.make_db([('a.txt', 'uploading')])  # the engine starts
        self.extension.poll()
        self.assertEqual(file.invalidated, 1)
        self.extension.update_file_info(file)
        self.assertEqual(file.label, 'Uploading')

    def test_connection_removed_while_running(self):
        work = self.add_work()
        self.extension.poll()
        home = self.annotate_in(self.extension, self.home, 'a.txt')
        file = self.annotate_in(self.extension, work, 'a.txt')
        self.assertEqual(file.label, 'Waiting to upload')
        self.extension.poll()
        # `connections remove` deletes config.toml first; the database stays
        # until the engine has stopped.
        work.remove_config()
        self.extension.poll()
        self.assertEqual((file.invalidated, home.invalidated), (1, 0))
        self.assertNotIn(work.abspath('a.txt'), self.extension._seen)
        again = self.annotate_in(self.extension, work, 'a.txt')
        self.assertEqual((again.emblems, again.attributes), ([], {}))
        self.assertEqual(self.extension.get_file_items([work.file('b.txt')]),
                         [])
        self.assertEqual(
            self.extension.get_background_items(work.file('', True)), [])
        # The other connections carry on.
        self.home.put('a.txt', 'pending')
        self.extension.poll()
        self.assertEqual(home.invalidated, 1)

    def test_connection_added_again(self):
        work = self.add_work()
        self.extension.poll()
        work.remove_config()
        self.extension.poll()
        file = self.annotate_in(self.extension, work, 'a.txt')
        self.assertEqual(file.attributes, {})
        work.write_config('nextcloud')
        self.extension.poll()
        self.assertEqual(file.invalidated, 1)
        self.assertEqual(self.annotate_in(self.extension, work, 'a.txt').label,
                         'Waiting to upload')

    def test_default_connection_removed(self):
        work = self.add_work()
        self.extension.poll()
        home = self.annotate_in(self.extension, self.home, 'a.txt')
        self.assertEqual(home.label, 'Uploaded')
        self.home.remove_config()   # `connections remove default`
        self.extension.poll()
        self.assertEqual(home.invalidated, 1)
        again = self.annotate_in(self.extension, self.home, 'a.txt')
        self.assertEqual(again.attributes, {})
        self.assertEqual(self.annotate_in(self.extension, work, 'a.txt').label,
                         'Waiting to upload')

    def test_connections_are_listed_on_the_tick_only(self):
        self.add_work()
        self.extension.poll()
        with mock.patch.object(ext, 'connection_names',
                               wraps=ext.connection_names) as listing:
            for index in range(20):
                self.extension.update_file_info(
                    FakeFile(os.path.join(self.work_root, 'f%d' % index)))
                self.extension.update_file_info(
                    FakeFile(os.path.join(self.tmp, 'f%d' % index)))
                self.extension.get_file_items(
                    [FakeFile(os.path.join(self.work_root, 'b.txt'))])
            listing.assert_not_called()
            self.extension._on_tick()
        listing.assert_called_once_with()

    def test_outside_files_touch_no_database(self):
        self.add_work()
        self.extension.poll()
        with mock.patch.object(ext.StateDB, '_query') as query:
            for index in range(10):
                self.extension.update_file_info(
                    FakeFile(os.path.join(self.tmp, 'Other', 'f%d' % index)))
        query.assert_not_called()

    def test_not_connections(self):
        root = os.path.join(self.tmp, 'config', 'connections')
        for name in ('con', 'Default', '.hidden', '-x', 'trail.'):
            self.folder(name, 'Folder ' + name, 'dropbox',
                        entries=[('a.txt', 'synced')])
        os.makedirs(os.path.join(root, 'no-config'))
        os.makedirs(os.path.join(root, 'dir-config', 'config.toml'))
        with open(os.path.join(root, 'stray.toml'), 'w') as f:
            f.write('sync_dir = "/x"\n')
        self.add_work()
        self.assertEqual(ext.connection_names(), ['work'])
        self.extension.poll()
        file = FakeFile(os.path.join(self.tmp, 'Folder con', 'a.txt'))
        self.extension.update_file_info(file)
        self.assertEqual(file.attributes, {})
        (column,) = self.extension.get_columns()
        # Google Drive and Nextcloud.
        self.assertEqual(column.label, 'CloudDrop')


class PathTests(unittest.TestCase):

    def test_named_connections_are_beside_the_default_one(self):
        env = {'CLOUDDROP_CONFIG': '/c/config.toml',
               'CLOUDDROP_DATA_DIR': '/d'}
        with mock.patch.dict(os.environ, env):
            self.assertEqual(ext.connection_config_path('default'),
                             '/c/config.toml')
            self.assertEqual(ext.connection_db_path('default'),
                             '/d/state.db')
            self.assertEqual(ext.connection_config_path('Work'),
                             '/c/connections/Work/config.toml')
            self.assertEqual(ext.connection_db_path('Work'),
                             '/d/connections/Work/state.db')

    def test_xdg_places(self):
        env = {'HOME': '/h', 'XDG_CONFIG_HOME': '/xc', 'XDG_DATA_HOME': '/xd'}
        with mock.patch.dict(os.environ, env):
            os.environ.pop('CLOUDDROP_CONFIG', None)
            os.environ.pop('CLOUDDROP_DATA_DIR', None)
            self.assertEqual(ext.connection_config_path('w'),
                             '/xc/clouddrop/connections/w/config.toml')
            self.assertEqual(ext.connection_db_path('w'),
                             '/xd/clouddrop/connections/w/state.db')
            os.environ.pop('XDG_CONFIG_HOME')
            os.environ.pop('XDG_DATA_HOME')
            self.assertEqual(
                ext.connection_config_path('w'),
                '/h/.config/clouddrop/connections/w/config.toml')
            self.assertEqual(
                ext.connection_db_path('w'),
                '/h/.local/share/clouddrop/connections/w/state.db')

    def test_names_as_the_cli_takes_them(self):
        # The cases of check_name's test in src/connections.rs.
        for ok in ('Work', 'photos-2026', 'My Laptop', 'a.b_c', 'Über', 'x',
                   'x' * 32, 'COMx', 'console'):
            self.assertTrue(ext.valid_connection_name(ok), ok)
        for bad in ('', ' lead', '-x', 'trail ', 'dot.', 'a/b', 'a\\b',
                    'a!b', 'default', 'DEFAULT', 'con', 'Nul.txt', 'COM1',
                    'lpt9', 'x' * 33):
            self.assertFalse(ext.valid_connection_name(bad), bad)


class OrderTests(ConnectionsTestCase):

    def test_sorted_by_name_ignoring_case(self):
        for name in ('gamma', 'Beta', 'alpha'):
            self.folder(name, name.title() + ' Folder', db=False)
        self.assertEqual(ext.connection_names(), ['alpha', 'Beta', 'gamma'])
        self.folder('default', 'CloudDrop', db=False)
        extension = self.new_extension()
        self.assertEqual([c.name for c in extension._connections],
                         ['default', 'alpha', 'Beta', 'gamma'])


if __name__ == '__main__':
    unittest.main()
