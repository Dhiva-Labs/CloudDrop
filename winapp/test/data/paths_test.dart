import 'package:clouddrop_app/data/paths.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('Windows', () {
    final env = {
      'USERPROFILE': r'C:\Users\Dhiva',
      'APPDATA': r'C:\Users\Dhiva\AppData\Roaming',
      'LOCALAPPDATA': r'C:\Users\Dhiva\AppData\Local',
      'PATH': r'C:\Windows;C:\Tools',
    };

    test('default locations (docs/WINDOWS.md)', () {
      final p = AppPaths.resolve(env: env, windows: true, exeDir: r'C:\Program Files\CloudDrop');
      expect(p.configPath, r'C:\Users\Dhiva\AppData\Roaming\clouddrop\config.toml');
      expect(p.dataDir, r'C:\Users\Dhiva\AppData\Local\clouddrop');
      expect(p.dbPath, r'C:\Users\Dhiva\AppData\Local\clouddrop\state.db');
      expect(p.statusPath, r'C:\Users\Dhiva\AppData\Local\clouddrop\status.json');
      expect(p.tokenPath, r'C:\Users\Dhiva\AppData\Local\clouddrop\token.json');
      expect(p.defaultSyncDir, r'C:\Users\Dhiva\CloudDrop');
      expect(
        p.cliBinary,
        'clouddrop.exe',
        reason: 'falls back to the bare name when nothing is found',
      );
    });

    test('overrides', () {
      final p = AppPaths.resolve(
        env: {
          ...env,
          'CLOUDDROP_CONFIG': r'D:\c.toml',
          'CLOUDDROP_DATA_DIR': r'D:\data',
          'CLOUDDROP_BIN': r'D:\bin\cd.exe',
        },
        windows: true,
      );
      expect(p.configPath, r'D:\c.toml');
      expect(p.dataDir, r'D:\data');
      expect(p.dbPath, r'D:\data\state.db');
      expect(p.cliBinary, r'D:\bin\cd.exe');
    });

    test('the settings folder is the one holding config.toml', () {
      final p = AppPaths.resolve(env: env, windows: true);
      expect(p.configDir, r'C:\Users\Dhiva\AppData\Roaming\clouddrop');
      final q = AppPaths.resolve(
        env: {...env, 'CLOUDDROP_CONFIG': r'D:\sandbox\cfg\config.toml'},
        windows: true,
      );
      expect(q.configDir, r'D:\sandbox\cfg');
    });

    test('Windows shows real paths', () {
      final p = AppPaths.resolve(env: env, windows: true);
      expect(p.pretty(r'C:\Users\Dhiva\CloudDrop'), r'C:\Users\Dhiva\CloudDrop');
    });

    test('absolute entry paths use backslashes', () {
      final p = AppPaths.resolve(env: env, windows: true);
      expect(
        p.absolute(r'C:\Users\Dhiva\CloudDrop', 'Photos/2026/a.jpg'),
        r'C:\Users\Dhiva\CloudDrop\Photos\2026\a.jpg',
      );
      expect(p.absolute(r'C:\Users\Dhiva\CloudDrop', ''), r'C:\Users\Dhiva\CloudDrop');
    });
  });

  group('Linux', () {
    test('default locations', () {
      final p = AppPaths.resolve(
        env: {'HOME': '/home/me', 'PATH': '/usr/bin'},
        windows: false,
        exeDir: '/nonexistent',
      );
      expect(p.configPath, '/home/me/.config/clouddrop/config.toml');
      expect(p.dataDir, '/home/me/.local/share/clouddrop');
      expect(p.defaultSyncDir, '/home/me/CloudDrop');
      expect(p.homeDir, '/home/me');
    });

    test('paths are shown with the profile folder as ~', () {
      final p = AppPaths.resolve(env: {'HOME': '/home/me'}, windows: false, exeDir: '/nonexistent');
      expect(p.pretty('/home/me/CloudDrop'), '~/CloudDrop');
      expect(p.pretty('/home/me'), '~');
      expect(p.pretty('/mnt/data/CloudDrop'), '/mnt/data/CloudDrop');
      expect(p.pretty('/home/meX/CloudDrop'), '/home/meX/CloudDrop');
    });

    test('XDG directories and overrides', () {
      final p = AppPaths.resolve(
        env: {'HOME': '/h', 'XDG_CONFIG_HOME': '/xc', 'XDG_DATA_HOME': '/xd'},
        windows: false,
        exeDir: '/nonexistent',
      );
      expect(p.configPath, '/xc/clouddrop/config.toml');
      expect(p.dataDir, '/xd/clouddrop');
      final q = AppPaths.resolve(
        env: {
          'HOME': '/h',
          'CLOUDDROP_CONFIG': '/s/c.toml',
          'CLOUDDROP_DATA_DIR': '/s/d',
          'CLOUDDROP_BIN': '/s/clouddrop',
        },
        windows: false,
      );
      expect((q.configPath, q.dataDir, q.cliBinary), ('/s/c.toml', '/s/d', '/s/clouddrop'));
      expect(q.absolute('/s/sync', 'a/b.txt'), '/s/sync/a/b.txt');
    });
  });
}
