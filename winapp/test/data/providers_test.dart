import 'package:clouddrop_app/data/cli_runner.dart';
import 'package:clouddrop_app/data/engine_api.dart';
import 'package:clouddrop_app/data/providers.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fakes.dart';

/// `clouddrop providers --json` as docs/PROVIDERS.md shows it, plus Google, an OAuth one and
/// a kind this app doesn't know yet.
const providersJson = '''
[{"id": "gdrive", "name": "Google Drive", "verify_by_download": false, "web_links": true,
  "sign_in": {"kind": "google"}},
 {"id": "onedrive", "name": "OneDrive", "verify_by_download": false, "web_links": true,
  "sign_in": {"kind": "oauth"}},
 {"id": "webdav", "name": "WebDAV", "verify_by_download": true, "web_links": false,
  "sign_in": {"kind": "form", "fields": [{"key": "url", "label": "Server address", "kind": "url",
                                          "required": true, "hint": "https://cloud.example.com/remote.php/dav/files/me"},
                                         {"key": "password", "label": "Password", "kind": "secret", "required": true, "hint": ""},
                                         {"key": "port", "label": "Port", "kind": "number", "required": false, "hint": ""}]}},
 {"id": "s3", "name": "S3", "web_links": false, "quota": false, "description": "Buckets.",
  "own_folder": true, "sign_in": {"kind": "form", "fields": []}},
 {"name": "no id, skipped"}]''';

void main() {
  group('providers --json', () {
    test('is parsed: kinds, fields, flags', () {
      final list = parseProviders(providersJson);
      expect([for (final p in list) p.id], ['gdrive', 'onedrive', 'webdav', 's3']);
      final g = list[0];
      expect(g, CloudProvider.google);
      expect(
        (g.isGoogle, g.rootLabel, g.signIn, g.webLinks),
        (true, 'My Drive', SignInKind.google, true),
      );
      expect(g.ownFolderOnly, isTrue);
      expect(g.description, "Google's cloud storage, with your Google account.");
      expect(g.stepLabel, 'Sign in');

      final o = list[1];
      expect(
        (o.name, o.signIn, o.rootLabel, o.isGoogle),
        ('OneDrive', SignInKind.oauth, 'OneDrive', false),
      );
      expect(o.ownFolderOnly, isTrue, reason: 'an app folder');

      final w = list[2];
      expect(
        (w.signIn, w.verifyByDownload, w.webLinks, w.hasQuota),
        (SignInKind.form, true, false, null),
      );
      expect(w.stepLabel, 'Connect');
      expect(w.ownFolderOnly, isFalse);
      expect(
        [for (final f in w.fields) f.kind],
        [FieldKind.url, FieldKind.secret, FieldKind.number],
      );
      expect(w.fields.first.hint, 'https://cloud.example.com/remote.php/dav/files/me');
      expect(w.fields.first.required, isTrue);

      final s = list[3];
      expect(s.hasQuota, isFalse);
      expect(s.description, 'Buckets.', reason: "the CLI's own description wins");
      expect(s.ownFolderOnly, isTrue, reason: "the CLI's own word wins");
    });

    test('unknown kinds and ids fall back sensibly', () {
      final p = parseProviders('[{"id": "floppy", "sign_in": {"kind": "telepathy"}}]').single;
      expect((p.name, p.signIn, p.description, p.webLinks), ('floppy', SignInKind.oauth, '', true));
      final f = parseProviders(
        '[{"id": "x", "name": "X", "sign_in": {"kind": "form", "fields": [{"key": "k", "kind": "?"}]}}]',
      ).single;
      expect(f.description, 'Connect with a few details.');
      expect(f.fields.single.kind, FieldKind.text);
      expect(f.fields.single.label, 'k');
      expect(() => parseProviders('{}'), throwsFormatException);
    });

    test('field checks: required, addresses, numbers', () {
      const url = ProviderField(key: 'url', label: 'Server', kind: FieldKind.url, required: true);
      expect(url.problem(''), 'Required');
      expect(url.problem('cloud.example.com'), 'Enter an address that starts with https://');
      expect(url.problem('ftp://x'), 'Enter an address that starts with https://');
      expect(url.problem('https://cloud.example.com/dav'), isNull);
      expect(url.problem('http://nas.local:8080'), isNull);
      const port = ProviderField(key: 'port', label: 'Port', kind: FieldKind.number);
      expect(port.problem(''), isNull, reason: 'optional');
      expect(port.problem('22'), isNull);
      expect(port.problem('22x'), 'Enter a number');
      const secret = ProviderField(key: 'p', label: 'P', kind: FieldKind.secret, required: true);
      expect(secret.problem(' '), 'Required');
      expect(secret.problem('s3cret'), isNull);
      expect(looksLikeUrl('https://'), isFalse);
    });
  });

  group('the CLI for other providers', () {
    late FakeCliRunner runner;
    late CliEngineApi api;
    setUp(() {
      runner = FakeCliRunner();
      api = CliEngineApi(runner);
    });

    test('providers --json, and just Google Drive from an engine that cannot list them', () async {
      runner.onRun = (a, e) => const CliResult(0, providersJson, '');
      expect((await api.providers()).map((p) => p.id), ['gdrive', 'onedrive', 'webdav', 's3']);
      expect(runner.calls.single, ['providers', '--json']);
      runner.onRun = (a, e) => const CliResult(2, '', "error: unrecognized subcommand 'providers'");
      expect(await api.providers(), [CloudProvider.google]);
      runner.onRun = (a, e) => const CliResult(0, 'not json', '');
      expect(await api.providers(), [CloudProvider.google]);
      runner.onRun = (a, e) => const CliResult(0, '[]', '');
      expect(await api.providers(), [CloudProvider.google]);
      runner.onRun = (a, e) => throw CliUnavailable('clouddrop', 'missing');
      expect(await api.providers(), [CloudProvider.google]);
    });

    test('setup --provider with --set and --secret; Google setup is unchanged', () async {
      await api.setup(
        folder: '/home/me/CloudDrop',
        driveFolder: 'CloudDrop',
        provider: 'webdav',
        settings: const {'url': 'https://nas/dav', 'user': 'me'},
        secrets: const {'password': 'p=w d'},
      );
      expect(runner.calls.last, [
        'setup',
        '--no-login',
        '--folder=/home/me/CloudDrop',
        '--drive-folder=CloudDrop',
        '--provider=webdav',
        '--set',
        'url=https://nas/dav',
        '--set',
        'user=me',
        '--secret',
        'password=p=w d',
      ]);
      await api.setup(folder: '/home/me/CloudDrop', driveFolder: 'CloudDrop');
      expect(runner.calls.last, [
        'setup',
        '--no-login',
        '--folder=/home/me/CloudDrop',
        '--drive-folder=CloudDrop',
      ]);
    });

    test('check, reset-cloud, service stop', () async {
      runner.onRun = (a, e) => a.first == 'check'
          ? const CliResult(1, '', 'clouddrop: The server refused the password (401).')
          : const CliResult(0, 'ok', '');
      final r = await api.check();
      expect((r.ok, r.message), (false, 'The server refused the password (401).'));
      await api.resetCloud();
      await api.serviceStop();
      expect(runner.calls, [
        ['check'],
        ['reset-cloud', '--yes'],
        ['service', 'stop'],
      ]);
    });

    test('status --json names the provider and the account', () async {
      runner.onRun = (a, e) => const CliResult(
        0,
        '{"configured": true, "provider": "onedrive", "provider_name": "OneDrive", '
            '"account": "dhiva@example.com", "signed_in": true}',
        '',
      );
      final s = await api.status();
      expect(
        (s.provider, s.providerName, s.account),
        ('onedrive', 'OneDrive', 'dhiva@example.com'),
      );
      runner.onRun = (a, e) => const CliResult(0, '{"configured": true, "account": ""}', '');
      final g = await api.status();
      expect((g.provider, g.providerName, g.account), (null, null, null));
    });

    test('config get keeps the providers\' own sections, for the setup assistant', () async {
      runner.onRun = (a, e) => const CliResult(
        0,
        '{"sync_dir": "/s", "drive_folder": "CloudDrop", "provider": "webdav", '
            '"webdav": {"url": "https://nas/dav", "user": "me", "port": 443}, '
            '"endpoints": {"api_base": "http://x"}}',
        '',
      );
      final c = (await api.configGet())!;
      expect(c.sections, {
        'webdav': {'url': 'https://nas/dav', 'user': 'me', 'port': '443'},
      });
    });
  });
}
