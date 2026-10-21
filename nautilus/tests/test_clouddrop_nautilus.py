import glob
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import support
from support import FakeFile, ExtensionTestCase, ext, no_binary_anywhere

SYNCED = (ext.EMBLEM_SYNCED, 'Uploaded')
UPLOADING = (ext.EMBLEM_BUSY, 'Uploading')
WAITING = (ext.EMBLEM_BUSY, 'Waiting to upload')


def names(items):
    return [item.name for item in items]


def labels(items):
    return [item.label for item in items]


def strings(items):
    """Every label and tip of a menu, submenus included, in order."""
    found = []
    for item in items:
        found += [item.label, item.tip]
        if item.submenu:
            found += strings(item.submenu.items)
    return found


class DataDirTests(unittest.TestCase):

    def test_env_override_wins(self):
        with mock.patch.dict(os.environ, {'CLOUDDROP_DATA_DIR': '/x/y',
                                          'XDG_DATA_HOME': '/z'}):
            self.assertEqual(ext.db_path(), '/x/y/state.db')

    def test_xdg_data_home(self):
        env = {'XDG_DATA_HOME': '/z', 'HOME': '/h'}
        with mock.patch.dict(os.environ, env):
            os.environ.pop('CLOUDDROP_DATA_DIR', None)
            self.assertEqual(ext.db_path(), '/z/clouddrop/state.db')

    def test_home_default(self):
        with mock.patch.dict(os.environ, {'HOME': '/h'}):
            os.environ.pop('CLOUDDROP_DATA_DIR', None)
            os.environ.pop('XDG_DATA_HOME', None)
            self.assertEqual(ext.db_path(),
                             '/h/.local/share/clouddrop/state.db')


class RelativePathTests(unittest.TestCase):

    def test_cases(self):
        rel = ext.relative_path
        self.assertEqual(rel('/r/Drop', '/r/Drop'), '')
        self.assertEqual(rel('/r/Drop', '/r/Drop/a/b.txt'), 'a/b.txt')
        self.assertEqual(rel('/r/Drop', '/r/Drop/a/../b'), 'b')
        self.assertIsNone(rel('/r/Drop', '/r/Drop2/a'))
        self.assertIsNone(rel('/r/Drop', '/r'))
        self.assertIsNone(rel('/r/Drop', '/elsewhere/a'))
        self.assertEqual(rel('/', '/a'), 'a')


class NoDatabaseTests(ExtensionTestCase):

    def check_quiet(self):
        extension = self.new_extension()
        file = self.file('a.txt')
        extension.update_file_info(file)
        self.assertEqual(file.emblems, [])
        self.assertEqual(file.attributes, {})
        self.assertEqual(extension.get_file_items([file]), [])
        self.assertEqual(
            extension.get_background_items(self.file('', is_dir=True)), [])
        extension.poll()
        extension._on_tick()

    def test_missing_database(self):
        self.check_quiet()

    def test_database_without_sync_root(self):
        self.make_db(sync_root='x')
        self.writer.conn.execute("DELETE FROM meta WHERE key='sync_root'")
        self.check_quiet()

    def test_not_a_database(self):
        with open(self.db_file, 'w') as handle:
            handle.write('this is not sqlite' * 100)
        self.check_quiet()

    def test_database_without_tables(self):
        sqlite3.connect(self.db_file).close()
        self.check_quiet()

    def test_unexpected_exception_is_contained(self):
        self.make_db([('a.txt', 'synced')])
        extension = self.new_extension()
        broken = FakeFile(self.abspath('a.txt'))
        broken.add_emblem = mock.Mock(side_effect=RuntimeError('boom'))
        extension.update_file_info(broken)   # must not raise
        with mock.patch.object(ext, 'plan_file_menu',
                               side_effect=RuntimeError('boom')):
            self.assertEqual(
                extension.get_file_items([self.file('a.txt')]), [])


class ColumnTests(ExtensionTestCase):

    def test_column(self):
        (column,) = self.new_extension().get_columns()
        self.assertEqual(column.attribute, 'clouddrop_status')
        self.assertEqual(column.label, 'Google Drive')

    def test_column_description_for_google_drive(self):
        (column,) = self.new_extension().get_columns()
        self.assertEqual(column.description, 'Upload status in Google Drive')

    def test_column_named_after_provider(self):
        self.use_config('provider = "dropbox"\n')
        (column,) = self.new_extension().get_columns()
        self.assertEqual(
            (column.name, column.label, column.description),
            ('CloudDrop::status', 'Dropbox', 'Upload status in Dropbox'))

    def test_timer_registered(self):
        support.StubGLib.timeouts.clear()
        extension = self.new_extension()
        interval, callback = support.StubGLib.timeouts[-1]
        self.assertEqual(interval, 2)
        self.assertEqual(callback, extension._on_tick)


class FileStatusTests(ExtensionTestCase):

    def status(self, rel, is_dir=False):
        file = self.annotate(self.new_extension(), rel, is_dir)
        return file

    def test_each_state(self):
        self.make_db([
            ('synced.txt', 'synced'),
            ('pending.txt', 'pending'),
            ('up.txt', 'uploading'),
            ('bad.txt', 'error', 'file', None, 'quota exceeded'),
            ('gone.txt', 'cloud_only'),
        ])
        cases = {
            'synced.txt': ([ext.EMBLEM_SYNCED], 'Uploaded'),
            'pending.txt': ([ext.EMBLEM_BUSY], 'Waiting to upload'),
            'up.txt': ([ext.EMBLEM_BUSY], 'Uploading'),
            'bad.txt': ([ext.EMBLEM_ERROR], 'Upload failed: quota exceeded'),
            'gone.txt': ([], ''),
            'unknown.txt': ([], ''),
        }
        for rel, (emblems, label) in cases.items():
            with self.subTest(rel):
                file = self.status(rel)
                self.assertEqual(file.emblems, emblems)
                self.assertEqual(file.label, label)

    def test_error_without_message(self):
        self.make_db([('bad.txt', 'error')])
        self.assertEqual(self.status('bad.txt').label, 'Upload failed')

    def test_long_multiline_error_is_flattened(self):
        self.make_db([('bad.txt', 'error', 'file', None,
                       'line one\nline two ' + 'x' * 500)])
        label = self.status('bad.txt').label
        self.assertNotIn('\n', label)
        self.assertLessEqual(len(label), len('Upload failed: ') + 200)
        self.assertTrue(label.startswith('Upload failed: line one line two'))

    def test_emblem_icons_exist_in_installed_themes(self):
        themes = glob.glob('/usr/share/icons/Yaru') + glob.glob(
            '/usr/share/icons/HighContrast')
        if not themes:
            self.skipTest('Yaru/HighContrast icon themes not installed')
        for name in (ext.EMBLEM_SYNCED, ext.EMBLEM_BUSY, ext.EMBLEM_ERROR):
            with self.subTest(name):
                found = [path for theme in themes for path in glob.glob(
                    '%s/*/emblems/%s.*' % (theme, name))]
                self.assertTrue(found, name)

    def test_state_is_read_live(self):
        self.make_db([('a.txt', 'pending')])
        extension = self.new_extension()
        self.assertEqual(self.annotate(extension, 'a.txt').label,
                         'Waiting to upload')
        self.put('a.txt', 'synced')
        self.assertEqual(self.annotate(extension, 'a.txt').label, 'Uploaded')


class OutsideRootTests(ExtensionTestCase):

    def setUp(self):
        super().setUp()
        self.make_db([('a.txt', 'synced')])
        self.extension = self.new_extension()

    def test_ignored_paths(self):
        paths = [
            os.path.join(self.tmp, 'a.txt'),
            os.path.join(self.tmp, 'CloudDrop2', 'a.txt'),
            self.root,   # the root itself
            os.path.dirname(self.tmp),
        ]
        for path in paths:
            with self.subTest(path):
                file = FakeFile(path, is_dir=True)
                self.extension.update_file_info(file)
                self.assertEqual(file.emblems, [])
                self.assertEqual(file.attributes, {})
        self.assertEqual(len(self.extension._seen), 0)

    def test_non_file_scheme_and_missing_location(self):
        for file in (FakeFile(self.abspath('a.txt'), scheme='smb'),
                     FakeFile(None)):
            self.extension.update_file_info(file)
            self.assertEqual(file.attributes, {})

    def test_menus_ignore_outside(self):
        outside = FakeFile(os.path.join(self.tmp, 'a.txt'))
        self.assertEqual(self.extension.get_file_items([outside]), [])
        self.assertEqual(
            self.extension.get_background_items(
                FakeFile(self.tmp, is_dir=True)), [])


class DirectoryAggregationTests(ExtensionTestCase):

    def dir_status(self, rel='d'):
        file = self.annotate(self.new_extension(), rel, is_dir=True)
        return (file.emblems, file.label)

    def test_synced_directory(self):
        self.make_db([('d', 'synced', 'dir'), ('d/a', 'synced'),
                      ('d/s', 'synced', 'dir'), ('d/s/b', 'synced')])
        self.assertEqual(self.dir_status(), ([ext.EMBLEM_SYNCED], 'Uploaded'))

    def test_pending_descendant(self):
        self.make_db([('d', 'synced', 'dir'), ('d/s', 'synced', 'dir'),
                      ('d/s/b', 'pending')])
        self.assertEqual(self.dir_status(),
                         ([ext.EMBLEM_BUSY], 'Waiting to upload'))

    def test_uploading_beats_pending(self):
        self.make_db([('d', 'synced', 'dir'), ('d/a', 'pending'),
                      ('d/b', 'uploading')])
        self.assertEqual(self.dir_status(), ([ext.EMBLEM_BUSY], 'Uploading'))

    def test_error_beats_pending_beats_synced(self):
        self.make_db([('d', 'synced', 'dir'), ('d/a', 'synced'),
                      ('d/b', 'pending'), ('d/s/c', 'error')])
        emblems, label = self.dir_status()
        self.assertEqual(emblems, [ext.EMBLEM_ERROR])
        self.assertEqual(label, 'Upload failed')

    def test_own_state_counts(self):
        self.make_db([('d', 'pending', 'dir'), ('d/a', 'synced')])
        self.assertEqual(self.dir_status()[0], [ext.EMBLEM_BUSY])
        self.put('d', 'error', 'dir', None, 'name clash')
        self.assertEqual(self.dir_status(),
                         ([ext.EMBLEM_ERROR], 'Upload failed: name clash'))

    def test_cloud_only_descendants_do_not_matter(self):
        self.make_db([('d', 'synced', 'dir'), ('d/a', 'cloud_only')])
        self.assertEqual(self.dir_status()[0], [ext.EMBLEM_SYNCED])

    def test_directory_not_in_database(self):
        self.make_db([('d/a', 'pending')])
        self.assertEqual(self.dir_status()[0], [ext.EMBLEM_BUSY])
        self.assertEqual(self.dir_status('other'), ([], ''))

    def test_prefix_siblings_are_not_descendants(self):
        # Neighbours of 'd/' ... 'd0' in sort order must not leak in.
        self.make_db([
            ('d', 'synced', 'dir'), ('d/a', 'synced'),
            ('d-old', 'error', 'dir'), ('d-old/x', 'error'),
            ('d.bak/x', 'error'), ('d0/x', 'error'), ('d0', 'pending', 'dir'),
            ('d a/x', 'pending'), ('dd/x', 'error'),
        ])
        self.assertEqual(self.dir_status(), ([ext.EMBLEM_SYNCED], 'Uploaded'))

    def test_range_query_is_used(self):
        self.make_db([('d', 'synced', 'dir')])
        db = ext.StateDB()
        self.addCleanup(db.close)
        clause, params = ext._subtree_clause('d')
        plan = db._query(
            'EXPLAIN QUERY PLAN SELECT state FROM entries WHERE ' + clause,
            params)
        self.assertIn('SEARCH', ' '.join(str(row) for row in plan))


class MenuTestCase(ExtensionTestCase):

    def setUp(self):
        super().setUp()
        self.make_db([
            ('synced.txt', 'synced', 'file', 'ID-SYNCED'),
            ('pending.txt', 'pending'),
            ('bad.txt', 'error', 'file', 'ID-BAD', 'boom'),
            ('plain', 'synced', 'dir', 'ID-PLAIN'),
            ('plain/a', 'synced'),
            ('broken', 'synced', 'dir'),
            ('broken/deep/x', 'error'),
            ('nodrive.txt', 'synced'),
        ])
        self.extension = self.new_extension()

    def file_items(self, *rels):
        files = [self.file(rel, is_dir=rel in ('plain', 'broken', ''))
                 for rel in rels]
        return self.extension.get_file_items(files)


class FileMenuTests(MenuTestCase):

    def test_synced_file(self):
        items = self.file_items('synced.txt')
        self.assertEqual(names(items), ['CloudDrop::free', 'CloudDrop::open'])
        self.assertEqual(labels(items),
                         ['Free Up Space', 'Open in Google Drive'])

    def test_pending_file_has_nothing(self):
        self.assertEqual(self.file_items('pending.txt'), [])

    def test_error_file_gets_retry_and_open(self):
        self.assertEqual(names(self.file_items('bad.txt')),
                         ['CloudDrop::open', 'CloudDrop::retry'])

    def test_directory_gets_free_open_and_keep(self):
        self.assertEqual(names(self.file_items('plain')),
                         ['CloudDrop::free', 'CloudDrop::open', 'CloudDrop::keep'])

    def test_directory_with_failed_descendant_gets_retry(self):
        items = self.file_items('broken')
        self.assertEqual(names(items), ['CloudDrop::free', 'CloudDrop::retry',
                                        'CloudDrop::keep'])

    def test_clean_directory_has_no_retry(self):
        self.assertNotIn('CloudDrop::retry', names(self.file_items('plain')))

    def test_open_needs_drive_id(self):
        self.assertEqual(names(self.file_items('nodrive.txt')),
                         ['CloudDrop::free'])

    def test_open_needs_single_selection(self):
        items = self.file_items('synced.txt', 'plain')
        self.assertEqual(names(items), ['CloudDrop::free'])

    def test_any_outside_item_disables_everything(self):
        outside = FakeFile(os.path.join(self.tmp, 'x.txt'))
        items = self.extension.get_file_items(
            [self.file('synced.txt'), outside])
        self.assertEqual(items, [])

    def test_unknown_file_has_nothing(self):
        self.assertEqual(self.file_items('new-file.txt'), [])

    def test_empty_selection(self):
        self.assertEqual(self.extension.get_file_items([]), [])

    def test_root_selected_opens_root_folder(self):
        items = self.file_items('')
        # bad.txt below the root is in error, so Retry is offered too.
        self.assertEqual(names(items), ['CloudDrop::free', 'CloudDrop::open',
                                        'CloudDrop::retry'])
        with mock.patch.object(ext, 'launch') as launch:
            items[1].activate()
        launch.assert_called_once_with(
            (self.binary, '--connection', 'default', 'open', self.root))

    def test_no_binary_no_items(self):
        with no_binary_anywhere(self):
            self.assertEqual(self.file_items('synced.txt'), [])


class KeepMenuTests(MenuTestCase):

    def keep_items(self, config_text):
        path = self.write_config(config_text)
        with mock.patch.dict(os.environ, {'CLOUDDROP_CONFIG': path}):
            return self.file_items('plain')

    def test_keep_offered_for_unmarked_folder(self):
        items = self.keep_items('sync_dir = "/x"\n')
        self.assertEqual(items[-1].label, 'Always Keep on This Computer')
        with mock.patch.object(ext, 'launch') as launch:
            items[-1].activate()
        launch.assert_called_once_with(
            (self.binary, '--connection', 'default', '--notify', 'keep',
             os.path.join(self.root, 'plain')))

    def test_unkeep_offered_for_marked_folder(self):
        items = self.keep_items('keep_local = ["plain"]\n')
        self.assertEqual(items[-1].name, 'CloudDrop::unkeep')
        self.assertEqual(items[-1].label, 'Allow Automatic Free Up Space')

    def test_nothing_when_a_parent_is_kept(self):
        path = self.write_config('keep_local = ["plain"]\n')
        with mock.patch.dict(os.environ, {'CLOUDDROP_CONFIG': path}):
            self.assertEqual(ext.keep_local_folders(), ['plain'])
            spec = ext.plan_file_menu(
                self.extension._db,
                [(os.path.join(self.root, 'plain/sub'), 'plain/sub', True)],
                self.binary)
        self.assertNotIn('CloudDrop::keep', [s.name for s in spec])
        self.assertNotIn('CloudDrop::unkeep', [s.name for s in spec])

    def test_files_and_multi_selection_get_no_keep(self):
        self.assertNotIn('CloudDrop::keep', names(self.file_items('synced.txt')))
        self.assertNotIn('CloudDrop::keep',
                         names(self.file_items('plain', 'broken')))

    def test_broken_config_is_ignored(self):
        items = self.keep_items('this is not toml [[[')
        self.assertEqual(items[-1].name, 'CloudDrop::keep')


class ProviderConfigTests(ExtensionTestCase):

    def provider(self, text):
        self.use_config(text)
        return ext.configured_provider()

    def test_google_drive_by_default(self):
        os.environ['CLOUDDROP_CONFIG'] = os.path.join(self.tmp, 'none.toml')
        self.assertEqual(ext.configured_provider(),
                         ext.Provider('gdrive', 'Google Drive', True))
        self.assertIs(self.provider('keep_local = ["a"]\n'), ext.GOOGLE_DRIVE)
        self.assertIs(self.provider('provider = "gdrive"\n'), ext.GOOGLE_DRIVE)

    def test_onedrive(self):
        self.assertEqual(self.provider('provider = "onedrive"\n'),
                         ext.Provider('onedrive', 'OneDrive', True))

    def test_every_known_provider(self):
        expected = {
            'gdrive': ('Google Drive', True), 'onedrive': ('OneDrive', True),
            'dropbox': ('Dropbox', True), 'nextcloud': ('Nextcloud', True),
            'webdav': ('WebDAV', False), 's3': ('S3', False),
            'sftp': ('SFTP', False), 'mega': ('MEGA', False),
        }
        for id_, (name, web_links) in expected.items():
            with self.subTest(id_):
                self.assertEqual(self.provider('provider = "%s"\n' % id_),
                                 (id_, name, web_links))

    def test_unknown_id_is_its_own_name_without_web_links(self):
        self.assertEqual(self.provider('provider = "floppy"\n'),
                         ext.Provider('floppy', 'floppy', False))

    def test_broken_config_means_google_drive(self):
        for text in ('provider = "onedrive"\nthis is not toml [[[',
                     'provider = 3\n', 'provider = ""\n',
                     'provider = ["onedrive"]\n'):
            with self.subTest(text):
                self.assertIs(self.provider(text), ext.GOOGLE_DRIVE)

    def test_undecodable_config_means_google_drive(self):
        path = self.use_config('')
        with open(path, 'wb') as f:
            f.write(b'provider = "one\xffdrive"\n')
        self.assertIs(ext.configured_provider(), ext.GOOGLE_DRIVE)

    def test_unreadable_config_means_google_drive(self):
        path = os.path.join(self.tmp, 'config.toml')
        os.makedirs(path)   # stat() works, open() fails
        os.environ['CLOUDDROP_CONFIG'] = path
        self.assertIs(ext.configured_provider(), ext.GOOGLE_DRIVE)

    def test_cache_follows_mtime(self):
        path = self.use_config('provider = "onedrive"\n')
        mtime = os.stat(path).st_mtime_ns
        self.assertEqual(ext.configured_provider().id, 'onedrive')
        with open(path, 'w') as f:
            f.write('provider = "dropbox"\n')
        os.utime(path, ns=(mtime, mtime))
        self.assertEqual(ext.configured_provider().id, 'onedrive')   # cached
        os.utime(path, ns=(mtime + 10**9, mtime + 10**9))
        self.assertEqual(ext.configured_provider().id, 'dropbox')
        os.remove(path)
        self.assertIs(ext.configured_provider(), ext.GOOGLE_DRIVE)


class ProviderMenuTests(MenuTestCase):
    """Menu wording and the Open items follow the configured provider."""

    def setUp(self):
        super().setUp()
        self.put('gone.txt', 'cloud_only')

    def menus(self, provider_id=None):
        """(file menu for synced.txt, background menu of the root)."""
        if provider_id:
            self.use_config('provider = "%s"\n' % provider_id)
        return (self.file_items('synced.txt'),
                self.extension.get_background_items(self.file('', True)))

    def test_google_drive_wording_is_unchanged(self):
        files, background = self.menus()
        self.assertEqual(strings(files), [
            'Free Up Space',
            'Remove local copies that are safely stored in Google Drive',
            'Open in Google Drive', 'Open this item in Google Drive'])
        self.assertEqual(strings(background), [
            'Cloud-only Files (1)',
            'Files kept in Google Drive but removed from this computer',
            'Download All', 'Download every cloud-only item in this folder',
            'gone.txt', 'Download gone.txt from Google Drive',
            'Open Folder in Google Drive', 'Open this folder in Google Drive'])
        explicit = self.menus('gdrive')
        self.assertEqual([strings(m) for m in explicit],
                         [strings(files), strings(background)])

    def test_onedrive(self):
        files, background = self.menus('onedrive')
        self.assertEqual(names(files), ['CloudDrop::free', 'CloudDrop::open'])
        self.assertEqual(strings(files), [
            'Free Up Space',
            'Remove local copies that are safely stored in OneDrive',
            'Open in OneDrive', 'Open this item in OneDrive'])
        self.assertEqual(strings(background), [
            'Cloud-only Files (1)',
            'Files kept in OneDrive but removed from this computer',
            'Download All', 'Download every cloud-only item in this folder',
            'gone.txt', 'Download gone.txt from OneDrive',
            'Open Folder in OneDrive', 'Open this folder in OneDrive'])

    def test_webdav_has_no_open_items(self):
        files, background = self.menus('webdav')
        self.assertEqual(names(files), ['CloudDrop::free'])
        self.assertEqual(names(background), ['CloudDrop::cloud_only'])
        self.assertEqual(strings(files), [
            'Free Up Space',
            'Remove local copies that are safely stored in WebDAV'])
        self.assertEqual(strings(background), [
            'Cloud-only Files (1)',
            'Files kept in WebDAV but removed from this computer',
            'Download All', 'Download every cloud-only item in this folder',
            'gone.txt', 'Download gone.txt from WebDAV'])

    def test_planners_take_the_provider(self):
        db = self.extension._db
        webdav = ext.PROVIDERS['webdav']
        spec = ext.plan_file_menu(
            db, [(self.abspath('synced.txt'), 'synced.txt', False)],
            self.binary, webdav)
        self.assertEqual([s.name for s in spec], ['CloudDrop::free'])
        spec = ext.plan_background_menu(db, self.root, '', self.binary, webdav)
        self.assertEqual([s.name for s in spec], ['CloudDrop::cloud_only'])


class CommandTests(MenuTestCase):

    def run_item(self, item):
        with mock.patch.object(subprocess, 'Popen') as popen:
            item.activate()
        return popen

    def test_free_argv_has_only_eligible_paths(self):
        items = self.file_items('synced.txt', 'pending.txt', 'plain')
        (free,) = items
        popen = self.run_item(free)
        popen.assert_called_once_with(
            [self.binary, '--connection', 'default', '--notify', 'free',
             self.abspath('synced.txt'), self.abspath('plain')],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True)

    def test_open_argv(self):
        free, open_ = self.file_items('synced.txt')
        self.assertEqual(self.run_item(open_).call_args.args[0],
                         [self.binary, '--connection', 'default', 'open',
                          self.abspath('synced.txt')])

    def test_retry_argv(self):
        items = self.file_items('bad.txt', 'broken', 'synced.txt')
        retry = [i for i in items if i.name == 'CloudDrop::retry'][0]
        self.assertEqual(
            self.run_item(retry).call_args.args[0],
            [self.binary, '--connection', 'default', '--notify', 'retry',
             self.abspath('bad.txt'), self.abspath('broken')])

    def test_popen_failure_is_swallowed(self):
        (free, _open) = self.file_items('synced.txt')
        with mock.patch.object(subprocess, 'Popen', side_effect=OSError):
            free.activate()

    def test_started_process_is_reaped(self):
        (free, _open) = self.file_items('synced.txt')
        with mock.patch.object(subprocess, 'Popen') as popen, \
                mock.patch.object(ext.threading, 'Thread') as thread:
            free.activate()
        thread.assert_called_once_with(
            target=popen.return_value.wait, daemon=True)
        thread.return_value.start.assert_called_once_with()

    def test_item_names_are_unique(self):
        self.make_db_children(30)
        items = self.extension.get_background_items(self.file('', True))
        seen = []

        def walk(item):
            seen.append(item.name)
            if item.submenu:
                for child in item.submenu.items:
                    walk(child)
        for item in items:
            walk(item)
        self.assertEqual(len(seen), len(set(seen)))

    def make_db_children(self, count):
        for index in range(count):
            self.put('c%02d' % index, 'cloud_only')


class BinaryLookupTests(ExtensionTestCase):

    def test_path_first(self):
        self.assertEqual(ext.find_binary(), self.binary)

    def test_local_bin_then_cargo_bin(self):
        home = os.path.join(self.tmp, 'home')
        empty = os.path.join(self.tmp, 'empty')
        os.makedirs(empty)
        cargo = self.make_binary('home/.cargo/bin')
        with mock.patch.dict(os.environ, {'PATH': empty, 'HOME': home}):
            self.assertEqual(ext.find_binary(), cargo)
            local = self.make_binary('home/.local/bin')
            self.assertEqual(ext.find_binary(), local)

    def test_non_executable_is_skipped(self):
        home = os.path.join(self.tmp, 'home')
        empty = os.path.join(self.tmp, 'empty')
        os.makedirs(empty)
        path = self.make_binary('home/.local/bin')
        os.chmod(path, 0o644)
        with mock.patch.dict(os.environ, {'PATH': empty, 'HOME': home}):
            self.assertIsNone(ext.find_binary())


class BackgroundMenuTests(ExtensionTestCase):

    def background(self, rel='', extension=None):
        extension = extension or self.new_extension()
        return extension.get_background_items(self.file(rel, is_dir=True))

    def test_no_cloud_only_no_submenu(self):
        self.make_db([('a', 'synced', 'dir', 'ID-A'), ('a/x', 'synced')])
        self.assertEqual(names(self.background()), ['CloudDrop::open_folder'])

    def test_submenu_counts_direct_children_only(self):
        self.make_db([
            ('top.txt', 'cloud_only'),
            ('gone-dir', 'cloud_only', 'dir'),
            ('keep', 'synced', 'dir'),
            ('keep/deep.txt', 'cloud_only'),
            ('present.txt', 'synced'),
        ])
        items = self.background()
        submenu = items[0]
        self.assertEqual(submenu.label, 'Cloud-only Files (2)')
        self.assertEqual(labels(submenu.submenu.items),
                         ['Download All', 'gone-dir', 'top.txt'])
        # Inside the subfolder the nested file is a direct child.
        sub_items = self.background('keep')
        self.assertEqual(sub_items[0].label, 'Cloud-only Files (1)')
        self.assertEqual(labels(sub_items[0].submenu.items),
                         ['Download All', 'deep.txt'])

    def test_ordering_dirs_first_then_name(self):
        self.make_db([
            ('b.txt', 'cloud_only'), ('A.txt', 'cloud_only'),
            ('c.txt', 'cloud_only'), ('zdir', 'cloud_only', 'dir'),
            ('Adir', 'cloud_only', 'dir'), ('mdir', 'cloud_only', 'dir'),
        ])
        children = self.background()[0].submenu.items[1:]
        self.assertEqual(labels(children), ['Adir', 'mdir', 'zdir',
                                            'A.txt', 'b.txt', 'c.txt'])

    def test_cap_of_25_with_more_item(self):
        self.make_db([('f%03d.txt' % i, 'cloud_only') for i in range(30)])
        submenu = self.background()[0]
        self.assertEqual(submenu.label, 'Cloud-only Files (30)')
        items = submenu.submenu.items
        self.assertEqual(len(items), 1 + 25 + 1)
        self.assertEqual(items[1].label, 'f000.txt')
        self.assertEqual(items[25].label, 'f024.txt')
        more = items[-1]
        self.assertEqual(more.label, '…and 5 more')
        self.assertFalse(more.sensitive)
        self.assertEqual(more.handlers, [])
        self.assertTrue(all(i.sensitive for i in items[:-1]))

    def test_exactly_25_has_no_more_item(self):
        self.make_db([('f%03d.txt' % i, 'cloud_only') for i in range(25)])
        items = self.background()[0].submenu.items
        self.assertEqual(len(items), 26)
        self.assertNotIn('more', labels(items)[-1])

    def test_restore_commands(self):
        self.make_db([('sub', 'synced', 'dir'),
                      ('sub/a b.txt', 'cloud_only'),
                      ('sub/dir', 'cloud_only', 'dir')])
        submenu = self.background('sub')[0].submenu.items
        popen = mock.Mock()
        with mock.patch.object(subprocess, 'Popen', popen):
            for item in submenu:
                item.activate()
        sub = self.abspath('sub')
        restore = [self.binary, '--connection', 'default', '--notify',
                   'restore']
        self.assertEqual(
            [call.args[0] for call in popen.call_args_list],
            [restore + [sub], restore + [sub + '/dir'],
             restore + [sub + '/a b.txt']])

    def test_open_folder_uses_folder_drive_id(self):
        self.make_db([('sub', 'synced', 'dir', 'ID-SUB'),
                      ('plain', 'synced', 'dir')])
        extension = self.new_extension()
        (item,) = self.background('sub', extension)
        self.assertEqual(item.label, 'Open Folder in Google Drive')
        with mock.patch.object(ext, 'launch') as launch:
            item.activate()
        launch.assert_called_once_with(
            (self.binary, '--connection', 'default', 'open',
             self.abspath('sub')))
        self.assertEqual(self.background('plain', extension), [])

    def test_root_uses_meta_root_id(self):
        self.make_db([('x.txt', 'cloud_only')], root_id='ROOT-ID')
        items = self.background('')
        self.assertEqual(names(items),
                         ['CloudDrop::cloud_only', 'CloudDrop::open_folder'])

    def test_root_without_root_id_has_no_open(self):
        self.make_db([], root_id=None)
        self.assertEqual(self.background(''), [])

    def test_outside_root_is_empty(self):
        self.make_db([('x.txt', 'cloud_only')])
        extension = self.new_extension()
        self.assertEqual(
            extension.get_background_items(FakeFile(self.tmp, True)), [])

    def test_underscores_are_escaped_for_mnemonics(self):
        self.make_db([('my_file.txt', 'cloud_only')])
        items = self.background()[0].submenu.items
        self.assertEqual(items[1].label, 'my__file.txt')
        # ... while the command keeps the real name.
        with mock.patch.object(ext, 'launch') as launch:
            items[1].activate()
        self.assertEqual(launch.call_args.args[0][-1],
                         self.abspath('my_file.txt'))

    def test_no_binary_no_items(self):
        self.make_db([('x.txt', 'cloud_only')])
        with no_binary_anywhere(self):
            self.assertEqual(self.background(), [])


class LiveRefreshTests(ExtensionTestCase):

    def setUp(self):
        super().setUp()
        self.make_db([('a.txt', 'synced'), ('b.txt', 'synced'),
                      ('d', 'synced', 'dir'), ('d/c.txt', 'synced')])
        self.extension = self.new_extension()
        self.files = {rel: self.annotate(self.extension, rel,
                                         is_dir=(rel == 'd'))
                      for rel in ('a.txt', 'b.txt', 'd', 'new.txt')}
        self.extension.poll()   # baseline pass
        self.reset_counts()

    def reset_counts(self):
        for file in self.files.values():
            file.invalidated = 0

    def invalidated(self):
        return sorted(rel for rel, f in self.files.items() if f.invalidated)

    def test_unchanged_database_costs_nothing(self):
        with mock.patch.object(ext, 'compute_status',
                               wraps=ext.compute_status) as compute:
            self.extension.poll()
            self.extension.poll()
        compute.assert_not_called()
        self.assertEqual(self.invalidated(), [])

    def test_only_changed_files_are_invalidated(self):
        self.put('a.txt', 'pending')
        self.put('other.txt', 'pending')   # not on screen
        self.extension.poll()
        self.assertEqual(self.invalidated(), ['a.txt'])
        self.extension.poll()
        self.assertEqual(self.files['a.txt'].invalidated, 1)

    def test_commit_that_changes_nothing_visible(self):
        self.put('other.txt', 'pending')
        self.extension.poll()
        self.assertEqual(self.invalidated(), [])

    def test_directory_follows_descendants(self):
        self.put('d/c.txt', 'error', 'file', None, 'bad')
        self.extension.poll()
        self.assertEqual(self.invalidated(), ['d'])
        self.assertEqual(self.extension._seen[self.abspath('d')].status.emblem,
                         ext.EMBLEM_ERROR)

    def test_new_entry_for_unannotated_path_gets_emblem(self):
        self.put('new.txt', 'pending')
        self.extension.poll()
        self.assertEqual(self.invalidated(), ['new.txt'])

    def test_gone_files_are_dropped_without_invalidation(self):
        self.files['a.txt'].gone = True
        self.put('b.txt', 'pending')
        self.extension.poll()
        self.assertNotIn(self.abspath('a.txt'), self.extension._seen)
        self.assertEqual(self.invalidated(), ['b.txt'])

    def test_reannotation_replaces_stale_object(self):
        newer = self.annotate(self.extension, 'a.txt')
        self.assertIs(self.extension._seen[self.abspath('a.txt')].file, newer)

    def test_tracking_is_capped_and_lru(self):
        with mock.patch.object(ext, 'MAX_TRACKED', 3):
            extension = self.new_extension()
            files = [self.annotate(extension, 'f%d' % i) for i in range(5)]
            self.assertEqual(len(extension._seen), 3)
            self.annotate(extension, 'f2')   # touch: now most recent
            self.annotate(extension, 'f9')
        self.assertEqual(
            list(extension._seen),
            [self.abspath(rel) for rel in ('f4', 'f2', 'f9')])
        self.assertEqual(len(files), 5)

    def test_busy_database_is_skipped_quietly(self):
        self.put('a.txt', 'pending')
        with mock.patch.object(ext.StateDB, 'data_version',
                               side_effect=ext.Unavailable('busy')):
            self.extension.poll()
        self.assertEqual(self.invalidated(), [])
        self.extension.poll()   # recovers on the next tick
        self.assertEqual(self.invalidated(), ['a.txt'])

    def test_work_per_tick_is_bounded(self):
        self.put('a.txt', 'pending')
        self.put('b.txt', 'pending')
        with mock.patch.object(ext, 'POLL_BUDGET_S', 0):
            self.extension.poll()
            self.assertEqual(len(self.extension._queue), 3)   # one per tick
            self.extension.poll()   # continues without a new commit
            self.assertEqual(len(self.extension._queue), 2)
            self.extension.poll()
            self.extension.poll()
        self.assertEqual(self.extension._queue, [])
        self.assertEqual(self.invalidated(), ['a.txt', 'b.txt'])

    def test_unknown_status_is_refreshed_later(self):
        extension = self.new_extension()
        with mock.patch.object(ext, 'compute_status',
                               side_effect=ext.Unavailable('locked')):
            file = self.annotate(extension, 'a.txt')
        self.assertEqual(file.emblems, [])
        self.assertEqual(file.attributes, {})
        extension.poll()
        self.assertEqual(file.invalidated, 1)

    def test_tick_never_raises_and_keeps_running(self):
        with mock.patch.object(self.extension, 'poll',
                               side_effect=RuntimeError('boom')):
            self.assertTrue(self.extension._on_tick())
        self.assertTrue(self.extension._on_tick())


class ReplacedDatabaseTests(ExtensionTestCase):

    def replace_database(self, entries):
        self.writer.close()
        old_inode = os.stat(self.db_file).st_ino
        os.rename(self.db_file, self.db_file + '.old')
        for suffix in ('-wal', '-shm'):
            if os.path.exists(self.db_file + suffix):
                os.remove(self.db_file + suffix)
        self.make_db(entries)
        self.assertNotEqual(os.stat(self.db_file).st_ino, old_inode)

    def test_lookup_reopens_after_replacement(self):
        self.make_db([('a.txt', 'synced')])
        extension = self.new_extension()
        self.assertEqual(self.annotate(extension, 'a.txt').label, 'Uploaded')
        generation = extension._db.generation
        self.replace_database([('a.txt', 'error', 'file', None, 'new db')])
        self.assertEqual(self.annotate(extension, 'a.txt').label,
                         'Upload failed: new db')
        self.assertEqual(extension._db.generation, generation + 1)

    def test_poll_refreshes_everything_after_replacement(self):
        self.make_db([('a.txt', 'synced'), ('b.txt', 'synced')])
        extension = self.new_extension()
        files = [self.annotate(extension, rel) for rel in ('a.txt', 'b.txt')]
        extension.poll()
        self.replace_database([('a.txt', 'pending'), ('b.txt', 'synced')])
        extension.poll()
        self.assertEqual([f.invalidated for f in files], [1, 0])

    def test_database_deleted_then_recreated(self):
        self.make_db([('a.txt', 'synced')])
        extension = self.new_extension()
        file = self.annotate(extension, 'a.txt')
        extension.poll()
        self.writer.close()
        for suffix in ('', '-wal', '-shm'):
            if os.path.exists(self.db_file + suffix):
                os.remove(self.db_file + suffix)
        extension.poll()   # nothing to read: quiet
        self.assertEqual(file.invalidated, 0)
        self.make_db([('a.txt', 'pending')])
        extension.poll()
        self.assertEqual(file.invalidated, 1)

    def test_sync_root_change_is_picked_up(self):
        self.make_db([('a.txt', 'synced')])
        extension = self.new_extension()
        self.assertEqual(self.annotate(extension, 'a.txt').label, 'Uploaded')
        self.writer.set_meta('sync_root', os.path.join(self.tmp, 'Other'))
        self.assertEqual(self.annotate(extension, 'a.txt').label, None)
        extension.poll()
        self.assertNotIn(self.abspath('a.txt'), extension._seen)


class BusyDatabaseTests(ExtensionTestCase):
    """A locked database means 'unknown', never an exception or a hang."""

    def test_locked_database_is_unknown_and_connection_survives(self):
        # Rollback-journal mode: an exclusive writer really blocks readers.
        writer = self.make_db([('a.txt', 'synced')], wal=False)
        extension = self.new_extension()
        self.assertEqual(self.annotate(extension, 'a.txt').label, 'Uploaded')
        generation = extension._db.generation

        writer.conn.execute('BEGIN EXCLUSIVE')
        try:
            started = time.monotonic()
            file = self.annotate(extension, 'a.txt')
            elapsed = time.monotonic() - started
            self.assertEqual(file.emblems, [])
            self.assertEqual(file.attributes, {})
            self.assertLess(elapsed, 1.0)
            self.assertEqual(
                extension.get_file_items([self.file('a.txt')]), [])
            extension.poll()
        finally:
            writer.conn.execute('ROLLBACK')

        self.assertEqual(self.annotate(extension, 'a.txt').label, 'Uploaded')
        self.assertEqual(extension._db.generation, generation)


class RealBindingsSmokeTests(unittest.TestCase):
    """Load the module against the installed Nautilus 4.1 typelib."""

    CODE = r"""
import os, sys
sys.path.insert(0, sys.argv[1])
import clouddrop_nautilus as m
e = m.CloudDropExtension()
(col,) = e.get_columns()
assert col.props.attribute == 'clouddrop_status', col
spec = m.MenuSpec('CloudDrop::t', 'a_b', 'tip', ('x',), True,
                  (m.MenuSpec('CloudDrop::u', 'more', sensitive=False),))
item = e._make_item(spec)
assert item.props.label == 'a__b'
assert [c.props.sensitive for c in item.props.menu.get_items()] == [False]
print('ok')
"""

    def test_real_gi(self):
        probe = subprocess.run(
            [sys.executable, '-c',
             "import gi; gi.require_version('Nautilus', '4.1')"],
            capture_output=True)
        if probe.returncode != 0:
            self.skipTest('Nautilus 4.1 typelib not available')
        with tempfile.TemporaryDirectory() as tmp:
            env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'),
                   'HOME': tmp, 'CLOUDDROP_DATA_DIR': tmp}
            result = subprocess.run(
                [sys.executable, '-c', self.CODE,
                 os.path.dirname(support.EXTENSION_PATH)],
                capture_output=True, text=True, env=env, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), 'ok')


if __name__ == '__main__':
    unittest.main()
