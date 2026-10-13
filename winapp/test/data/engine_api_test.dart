import 'dart:convert';
import 'dart:io';

import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/models.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;

import '../support/fakes.dart';

const statusJson = '''
{
  "configured": true,
  "paused": true,
  "root_id": "ROOT123",
  "folder": "/home/me/CloudDrop",
  "drive_folder": "CloudDrop",
  "signed_in": true,
  "has_builtin_client": true,
  "daemon": {"pid": 1234, "updated_ms": 1791913161130, "phase": "uploading", "message": null,
    "uploads": [{"path": "Photos/beach.jpg", "sent": 524288, "total": 3000000}], "waiting": 4,
    "quota": {"limit": 16106127360, "usage": 4000, "usage_in_drive": 3000}},
  "files": {"synced": 5, "pending": 2, "uploading": 1, "cloud_only": 3, "error": 1},
  "cloud_bytes": 123456,
  "cloud_only_bytes": 777,
  "errors": [{"path": "x/y.bin", "error": "403: quota"}]
}''';

const configJson = '''
{
  "sync_dir": "/home/me/CloudDrop", "drive_folder": "Backup", "client_id": "1.apps.googleusercontent.com",
  "client_secret": true, "parallel_uploads": 3, "settle_secs": 5, "rescan_secs": 600, "notifications": false,
  "ignore": ["*.tmp", "*.part"], "auto_free_days": 30, "keep_local": ["Work", "Photos/2026"],
  "config_path": "/home/me/.config/clouddrop/config.toml", "data_dir": "/home/me/.local/share/clouddrop"
}''';

void main() {
  group('CliResult', () {
    test('success shows stdout', () {
      const r = CliResult(0, 'Uploads paused.\n', '');
      expect(r.ok, isTrue);
      expect(r.message, 'Uploads paused.');
    });

    test('failure shows the engine\'s own sentence, not the log lines before it', () {
      const r = CliResult(
        1,
        '',
        '2026-10-14T01:02:03Z  INFO clouddrop: starting\nclouddrop: Google rejected the saved sign-in\n',
      );
      expect(r.errorMessage, 'Google rejected the saved sign-in');
      expect(r.message, 'Google rejected the saved sign-in');
    });

    test('falls back to the last stderr line, stdout, or the exit code', () {
      expect(const CliResult(2, '', 'error: bad flag\n').errorMessage, 'error: bad flag');
      expect(const CliResult(1, 'only stdout', '').errorMessage, 'only stdout');
      expect(const CliResult(3, '', '').errorMessage, 'clouddrop exited with code 3');
    });
  });

  group('status and settings', () {
    late FakeCliRunner runner;
    late CliEngineApi api;
    setUp(() {
      runner = FakeCliRunner();
      api = CliEngineApi(runner);
    });

    test('status --json is parsed', () async {
      runner.onRun = (args, env) => const CliResult(0, statusJson, '');
      final s = await api.status();
      expect(runner.calls.single, ['status', '--json']);
      expect(s.configured && s.signedIn && s.hasBuiltinClient && s.ready, isTrue);
      expect(s.folder, '/home/me/CloudDrop');
      expect(s.daemon!.phase, Phase.uploading);
      expect(s.daemon!.uploads.single.path, 'Photos/beach.jpg');
      expect(s.daemon!.quota!.limit, 16106127360);
      expect(s.files['cloud_only'], 3);
      expect(s.cloudBytes, 123456);
      expect(s.errors.single.error, '403: quota');
    });

    test('a null daemon means the engine is not running', () async {
      runner.onRun = (a, e) => const CliResult(
        0,
        '{"folder":"/x","drive_folder":"D","signed_in":false,"has_builtin_client":false,"daemon":null,"files":{},"cloud_bytes":0,"cloud_only_bytes":0,"errors":[]}',
        '',
      );
      final s = await api.status();
      expect(s.daemon, isNull);
      expect(s.signedIn, isFalse);
      expect(s.ready, isFalse);
    });

    test('before setup the engine says so itself, with the built-in client flag', () async {
      runner.onRun = (args, env) => const CliResult(
        0,
        '{"configured": false, "signed_in": false, "has_builtin_client": true, "daemon": null}',
        '',
      );
      final s = await api.status();
      expect(runner.calls.single, ['status', '--json'], reason: 'no probing, no second command');
      expect(s.configured, isFalse);
      expect(s.ready, isFalse);
      expect(s.hasBuiltinClient, isTrue);
      expect(s.signedIn, isFalse);
      expect(s.daemon, isNull);
      expect(s.paused, isFalse);
      expect(s.rootId, isNull);
    });

    test('a sign-in left over without a config is reported as is', () async {
      runner.onRun = (args, env) => const CliResult(
        0,
        '{"configured": false, "signed_in": true, "has_builtin_client": false, "daemon": null}',
        '',
      );
      final s = await api.status();
      expect((s.configured, s.signedIn, s.hasBuiltinClient, s.ready), (false, true, false, false));
    });

    test('configured: paused and the Drive folder id come with it', () async {
      runner.onRun = (args, env) => const CliResult(0, statusJson, '');
      final s = await api.status();
      expect(s.configured, isTrue);
      expect(s.paused, isTrue);
      expect(s.rootId, 'ROOT123');
    });

    test('an engine that does not say "configured" is taken as configured', () async {
      runner.onRun = (args, env) => const CliResult(
        0,
        '{"folder":"/x","drive_folder":"D","signed_in":true,"has_builtin_client":false,"daemon":null,"files":{},"cloud_bytes":0,"cloud_only_bytes":0,"errors":[]}',
        '',
      );
      expect((await api.status()).configured, isTrue);
    });

    test('other failures surface as errors', () async {
      runner.onRun = (a, e) => const CliResult(1, '', 'clouddrop: invalid config /x: bad toml');
      expect(
        api.status(),
        throwsA(isA<StateError>().having((e) => e.message, 'message', contains('invalid config'))),
      );
    });

    test('config get --json', () async {
      runner.onRun = (a, e) => const CliResult(0, configJson, '');
      final c = (await api.configGet())!;
      expect(runner.calls.single, ['config', 'get', '--json']);
      expect((c.parallelUploads, c.settleSecs, c.notifications), (3, 5, false));
      expect(c.autoFreeDays, 30);
      expect(c.keepLocal, ['Work', 'Photos/2026']);
      expect(c.ignore, ['*.tmp', '*.part']);
      expect(c.driveFolder, 'Backup');
      expect(c.hasClient, isTrue);
      expect(c.hasClientSecret, isTrue);
    });

    test('config get without auto_free_days means never', () async {
      runner.onRun = (a, e) => CliResult(
        0,
        jsonEncode({
          'sync_dir': '/x',
          'client_id': 'a',
          'client_secret': false,
          'auto_free_days': null,
        }),
        '',
      );
      final c = (await api.configGet())!;
      expect(c.autoFreeDays, isNull);
      expect(c.hasClient, isFalse, reason: 'no secret stored');
    });

    test('config get fails quietly when there is no config', () async {
      runner.onRun = (a, e) => const CliResult(1, '', 'clouddrop: no config');
      expect(await api.configGet(), isNull);
    });

    test('config set passes key and value', () async {
      runner.onRun = (a, e) => const CliResult(0, 'parallel_uploads updated.', '');
      final r = await api.configSet('parallel_uploads', '4');
      expect(runner.calls.single, ['config', 'set', 'parallel_uploads', '4']);
      expect((r.ok, r.message), (true, 'parallel_uploads updated.'));
    });

    test('service status --json', () async {
      runner.onRun = (a, e) => const CliResult(0, '{"at_login":true,"running":true}\n', '');
      expect(await api.serviceStatus(), const ServiceStatus(running: true, startsAtSignIn: true));
      expect(runner.calls.last, ['service', 'status', '--json']);
      runner.onRun = (a, e) => const CliResult(0, '{"at_login":false,"running":true}', '');
      expect(await api.serviceStatus(), const ServiceStatus(running: true, startsAtSignIn: false));
      runner.onRun = (a, e) => const CliResult(0, '{"at_login":true,"running":false}', '');
      expect(await api.serviceStatus(), const ServiceStatus(running: false, startsAtSignIn: true));
    });

    test('service status that cannot be read counts as stopped', () async {
      const off = ServiceStatus(running: false, startsAtSignIn: false);
      runner.onRun = (a, e) => const CliResult(1, '', 'clouddrop: boom');
      expect(await api.serviceStatus(), off);
      runner.onRun = (a, e) => const CliResult(0, 'running, starts at sign-in', '');
      expect(await api.serviceStatus(), off, reason: 'the old text form is not parsed');
      runner.onRun = (a, e) => throw CliUnavailable('clouddrop', 'not found');
      expect(await api.serviceStatus(), off);
    });
  });

  group('actions', () {
    late FakeCliRunner runner;
    late CliEngineApi api;
    setUp(() {
      runner = FakeCliRunner();
      api = CliEngineApi(runner);
    });

    test('each action is one CLI command', () async {
      runner.onRun = (a, e) => CliResult(0, 'ok ${a.join(' ')}', '');
      await api.free(['/s/a', '/s/b']);
      await api.restore(['/s/a']);
      await api.retry(const []);
      await api.retry(['/s/c']);
      await api.pause();
      await api.resume();
      await api.keep(['/s/Work']);
      await api.unkeep(['/s/Work']);
      await api.serviceEnable();
      await api.serviceDisable();
      await api.serviceStart();
      await api.logout();
      await api.driveUrl('/s/a');
      expect(runner.calls, [
        ['free', '/s/a', '/s/b'],
        ['restore', '/s/a'],
        ['retry'],
        ['retry', '/s/c'],
        ['pause'],
        ['resume'],
        ['keep', '/s/Work'],
        ['unkeep', '/s/Work'],
        ['service', 'enable'],
        ['service', 'disable'],
        ['service', 'start'],
        ['logout'],
        ['url', '/s/a'],
      ]);
    });

    test('failures carry the error sentence', () async {
      runner.onRun = (a, e) =>
          const CliResult(1, '', 'clouddrop: /x is not inside the CloudDrop folder (/s)');
      final r = await api.free(['/x']);
      expect(r.ok, isFalse);
      expect(r.message, '/x is not inside the CloudDrop folder (/s)');
    });

    test('a missing CLI is reported, not thrown, for actions', () async {
      runner.onRun = (a, e) => throw CliUnavailable('clouddrop.exe', 'No such file');
      final r = await api.pause();
      expect(r.ok, isFalse);
      expect(r.message, contains('clouddrop.exe'));
    });

    test(
      'setup: arguments, and a typed key travels in a temp file, not on the command line',
      () async {
        String? jsonText;
        String? jsonPath;
        runner.onRun = (args, env) {
          final j = args.firstWhere((a) => a.startsWith('--client-json='), orElse: () => '');
          if (j.isNotEmpty) {
            jsonPath = j.substring('--client-json='.length);
            jsonText = File(jsonPath!).readAsStringSync();
          }
          return const CliResult(0, 'Config written', '');
        };
        final r = await api.setup(
          folder: r'/home/me/My Drive Stuff',
          driveFolder: '-weird name',
          clientId: '123-abc.apps.googleusercontent.com',
          clientSecret: 'GOCSPX-secret',
        );
        expect(r.ok, isTrue);
        final args = runner.calls.single;
        expect(args.take(2), ['setup', '--no-login']);
        expect(args, contains('--folder=/home/me/My Drive Stuff'));
        expect(args, contains('--drive-folder=-weird name'));
        expect(args.join(' '), isNot(contains('GOCSPX-secret')));
        expect(jsonDecode(jsonText!), {
          'installed': {
            'client_id': '123-abc.apps.googleusercontent.com',
            'client_secret': 'GOCSPX-secret',
          },
        });
        expect(
          File(jsonPath!).existsSync(),
          isFalse,
          reason: 'the temp key file is deleted afterwards',
        );
      },
    );

    test(
      'setup with a chosen key file passes it through; with a built-in client passes no key',
      () async {
        runner.onRun = (a, e) => const CliResult(0, '', '');
        await api.setup(
          folder: '/f',
          driveFolder: 'CloudDrop',
          clientJsonPath: '/dl/client_secret.json',
        );
        expect(runner.calls.last, contains('--client-json=/dl/client_secret.json'));
        await api.setup(folder: '/f', driveFolder: 'CloudDrop');
        expect(runner.calls.last.where((a) => a.startsWith('--client')), isEmpty);
      },
    );
  });

  group('sign-in (login --no-browser)', () {
    late FakeCliRunner runner;
    late CliEngineApi api;
    setUp(() {
      runner = FakeCliRunner();
      api = CliEngineApi(runner);
    });

    test('extracts the link, waits for exit 0', () async {
      final session = api.login();
      await pumpEventQueue();
      expect(runner.calls.single, ['login', '--no-browser']);
      final proc = runner.processes.single;
      proc.say('Sign in to Google to let CloudDrop upload to your Drive:');
      proc.say('');
      proc.say('  https://accounts.google.com/o/oauth2/v2/auth?client_id=abc&state=xyz');
      proc.say('Waiting for the browser…');
      expect(
        await session.url,
        'https://accounts.google.com/o/oauth2/v2/auth?client_id=abc&state=xyz',
      );
      var done = false;
      session.done.then((_) => done = true);
      await pumpEventQueue();
      expect(done, isFalse, reason: 'still waiting for the browser');
      proc.finish(0);
      final r = await session.done;
      expect((r.ok, r.message), (true, 'Signed in.'));
    });

    test('a failed sign-in reports the engine\'s sentence', () async {
      final session = api.login();
      await pumpEventQueue();
      final proc = runner.processes.single;
      proc.say('  http://127.0.0.1:8299/auth?x=1');
      proc.stderr = 'clouddrop: Google said: access_denied\n';
      proc.finish(1);
      final r = await session.done;
      expect(r.ok, isFalse);
      expect(r.message, 'Google said: access_denied');
    });

    test('the process ending before printing a link fails the link future too', () async {
      final session = api.login();
      await pumpEventQueue();
      final proc = runner.processes.single;
      proc.stderr = 'clouddrop: no config at /x\n';
      proc.finish(1);
      await expectLater(
        session.url,
        throwsA(isA<StateError>().having((e) => e.message, 'm', contains('no config'))),
      );
      expect((await session.done).ok, isFalse);
    });

    test('cancel kills the process', () async {
      final session = api.login();
      await pumpEventQueue();
      final proc = runner.processes.single;
      proc.say('https://example.test/auth');
      await session.url;
      session.cancel();
      final r = await session.done;
      expect(proc.killed, isTrue);
      expect(r.ok, isFalse);
      expect(r.message, 'Sign-in cancelled.');
    });

    test('a missing CLI fails cleanly', () async {
      runner.startError = CliUnavailable('clouddrop', 'not found');
      final session = api.login();
      final r = await session.done;
      expect(r.ok, isFalse);
      expect(r.message, contains('not found'));
      expect(session.url, throwsA(anything));
    });

    test('link extraction', () {
      expect(extractUrl('  https://a.test/x?y=1&z=2  '), 'https://a.test/x?y=1&z=2');
      expect(extractUrl('Waiting for the browser…'), isNull);
      expect(extractUrl('open http://127.0.0.1:8099/auth now'), 'http://127.0.0.1:8099/auth');
    });
  });

  group(
    'ProcessCliRunner runs a real process',
    () {
      late Directory tmp;
      late String script;

      setUp(() {
        tmp = Directory.systemTemp.createTempSync('clouddrop-cli');
        script = p.join(tmp.path, 'fake-clouddrop');
        File(script).writeAsStringSync(r'''#!/bin/sh
case "$1" in
  echo) printf '%s\n' "$2"; printf 'CONFIG=%s DATA=%s\n' "$CLOUDDROP_CONFIG" "$CLOUDDROP_DATA_DIR" ;;
  fail) echo "log noise" >&2; echo "clouddrop: it broke" >&2; exit 1 ;;
  utf8) printf 'Copied 1 item to Fotos für Zoë ☕.\n' ;;
  login) printf 'Sign in:\n\n  https://example.test/auth?x=1\n\nWaiting for the browser…\n'; sleep 0.2; exit 0 ;;
  slow) sleep 5 ;;
esac
''');
        Process.runSync('chmod', ['+x', script]);
      });
      tearDown(() => tmp.deleteSync(recursive: true));

      test('stdout, exit codes and stderr', () async {
        final runner = ProcessCliRunner(
          script,
          environment: {'CLOUDDROP_CONFIG': '/c', 'CLOUDDROP_DATA_DIR': '/d'},
        );
        final ok = await runner.run(['echo', 'hello world']);
        expect(ok.ok, isTrue);
        expect(ok.stdout, contains('hello world'));
        expect(ok.stdout, contains('CONFIG=/c DATA=/d'));
        final fail = await runner.run(['fail']);
        expect(fail.ok, isFalse);
        expect(fail.message, 'it broke');
      });

      test('per-call environment overrides win', () async {
        final runner = ProcessCliRunner(script, environment: {'CLOUDDROP_CONFIG': '/c'});
        final r = await runner.run(['echo', 'x'], environment: {'CLOUDDROP_CONFIG': '/other'});
        expect(r.stdout, contains('CONFIG=/other'));
      });

      test('output is read as UTF-8', () async {
        final r = await ProcessCliRunner(script).run(['utf8']);
        expect(r.message, 'Copied 1 item to Fotos für Zoë ☕.');
      });

      test('a missing binary is CliUnavailable', () async {
        expect(
          ProcessCliRunner(p.join(tmp.path, 'nope')).run(['x']),
          throwsA(isA<CliUnavailable>()),
        );
        expect(
          ProcessCliRunner(p.join(tmp.path, 'nope')).start(['x']),
          throwsA(isA<CliUnavailable>()),
        );
      });

      test('timeouts give exit 124', () async {
        final r = await ProcessCliRunner(
          script,
        ).run(['slow'], timeout: const Duration(milliseconds: 200));
        expect(r.exitCode, 124);
      });

      test('login --no-browser through the real runner', () async {
        final api = CliEngineApi(ProcessCliRunner(script));
        final session = api.login();
        expect(await session.url, 'https://example.test/auth?x=1');
        expect((await session.done).ok, isTrue);
      });
    },
    skip: Platform.isWindows ? 'uses a POSIX shell script' : false,
  );
}
