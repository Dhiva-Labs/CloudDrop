/// The clouds CloudDrop can upload to, as `clouddrop providers --json` lists them
/// (docs/PROVIDERS.md, "What the apps read"; `gui/src/providers.rs`).
library;

import 'dart:convert';

import 'package:flutter/foundation.dart';

/// How someone connects CloudDrop to a provider (`sign_in.kind`).
enum SignInKind {
  /// Google's own flow: a key of your own unless the build has one, then the browser.
  google,

  /// OAuth in the browser (`login --no-browser`), like Google without the key.
  oauth,

  /// Nextcloud/ownCloud Login Flow: the server's address, then the browser.
  nextcloud,

  /// Fields to fill in, then Test Connection (`check`).
  form;

  /// A kind this app doesn't know yet is taken as a browser sign-in (`login --no-browser`).
  static SignInKind parse(Object? s) => switch (s) {
    null || 'google' => google,
    'nextcloud' => nextcloud,
    'form' => form,
    _ => oauth,
  };
}

enum FieldKind {
  text,
  url,
  number,

  /// Never shown again; saved apart from the settings (`--secret`).
  secret,

  /// A path to a file on this computer (an SSH key).
  file;

  static FieldKind parse(Object? s) => switch (s) {
    'url' => url,
    'number' => number,
    'secret' => secret,
    'file' => file,
    _ => text,
  };
}

@immutable
class ProviderField {
  const ProviderField({
    required this.key,
    required this.label,
    this.kind = FieldKind.text,
    this.required = false,
    this.hint = '',
  });

  final String key;
  final String label;
  final FieldKind kind;
  final bool required;

  /// Example or explanation shown under the field.
  final String hint;

  factory ProviderField.fromJson(Map<String, dynamic> j) => ProviderField(
    key: '${j['key'] ?? ''}',
    label: '${j['label'] ?? j['key'] ?? ''}',
    kind: FieldKind.parse(j['kind']),
    required: j['required'] == true,
    hint: '${j['hint'] ?? ''}',
  );

  /// Why [value] can't be saved yet, or null when it can.
  String? problem(String value) {
    final v = value.trim();
    if (v.isEmpty) return required ? 'Required' : null;
    switch (kind) {
      case FieldKind.url:
        return looksLikeUrl(v) ? null : 'Enter an address that starts with https://';
      case FieldKind.number:
        return RegExp(r'^\d+$').hasMatch(v) ? null : 'Enter a number';
      case FieldKind.text || FieldKind.secret || FieldKind.file:
        return null;
    }
  }

  @override
  bool operator ==(Object other) =>
      other is ProviderField &&
      other.key == key &&
      other.label == label &&
      other.kind == kind &&
      other.required == required &&
      other.hint == hint;

  @override
  int get hashCode => Object.hash(key, label, kind, required, hint);
}

bool looksLikeUrl(String s) {
  final u = Uri.tryParse(s.trim());
  return u != null && (u.scheme == 'https' || u.scheme == 'http') && u.host.isNotEmpty;
}

/// One-line descriptions and "only its own folder" for the providers this app knows, used
/// when the CLI's list doesn't carry its own.
const Map<String, ({String text, bool ownFolder})> _known = {
  'gdrive': (text: "Google's cloud storage, with your Google account.", ownFolder: true),
  'onedrive': (
    text: "Microsoft's cloud storage, with a personal or work account.",
    ownFolder: true,
  ),
  'dropbox': (text: 'Dropbox, with your Dropbox account.', ownFolder: true),
  'nextcloud': (text: 'Your own Nextcloud or ownCloud server.', ownFolder: false),
  'webdav': (text: 'Any WebDAV server, such as a NAS.', ownFolder: false),
  's3': (
    text: 'Amazon S3 or a compatible bucket: Backblaze B2, Wasabi, Cloudflare R2, MinIO…',
    ownFolder: false,
  ),
  'sftp': (text: 'Any computer or server you can reach over SSH.', ownFolder: false),
  'mega': (text: "MEGA's end-to-end encrypted cloud storage.", ownFolder: false),
  // Offered only by builds made with CLOUDDROP_EXPERIMENTAL_PROVIDERS=1 (never the store's).
  'proton': (
    text: "Experimental: Proton Drive through Proton's own command-line app, which signs in. "
        'This is a third-party application not officially supported by Proton.',
    ownFolder: false,
  ),
};

@immutable
class CloudProvider {
  const CloudProvider({
    required this.id,
    required this.name,
    this.signIn = SignInKind.google,
    this.fields = const [],
    this.verifyByDownload = false,
    this.webLinks = true,
    this.hasQuota,
    this.customDescription,
    this.ownFolder,
  });

  /// `provider` in config.toml and `status --json` (`gdrive`, `onedrive`, …).
  final String id;

  /// The name people know it by ("Google Drive").
  final String name;
  final SignInKind signIn;

  /// The form's fields ([SignInKind.form]).
  final List<ProviderField> fields;

  /// Freeing space proves the cloud copy by downloading it ("Verifying…").
  final bool verifyByDownload;

  /// Files have a web page ("Open in OneDrive").
  final bool webLinks;

  /// Whether the provider reports its storage; null = the CLI didn't say.
  final bool? hasQuota;

  /// The CLI's own one-line description, when it gives one.
  final String? customDescription;

  /// The CLI says whether CloudDrop sees only its own folder; null = it didn't.
  final bool? ownFolder;

  static const google = CloudProvider(id: 'gdrive', name: 'Google Drive');

  bool get isGoogle => id == google.id;

  /// Where the top CloudDrop folder appears: "My Drive" or the provider's name.
  String get rootLabel => isGoogle ? 'My Drive' : name;

  String get description =>
      customDescription ??
      _known[id]?.text ??
      (signIn == SignInKind.form ? 'Connect with a few details.' : '');

  /// CloudDrop can only see the files it created (Drive's `drive.file`, an app folder).
  bool get ownFolderOnly => ownFolder ?? _known[id]?.ownFolder ?? false;

  /// The sign-in step's header label.
  String get stepLabel => signIn == SignInKind.form ? 'Connect' : 'Sign in';

  factory CloudProvider.fromJson(Map<String, dynamic> j) {
    final s = j['sign_in'];
    final signIn = s is Map ? s.cast<String, dynamic>() : const <String, dynamic>{};
    final fields = signIn['fields'];
    final id = '${j['id'] ?? ''}';
    return CloudProvider(
      id: id,
      name: '${j['name'] ?? id}',
      signIn: SignInKind.parse(signIn['kind']),
      fields: [
        if (fields is List)
          for (final f in fields)
            if (f is Map) ProviderField.fromJson(f.cast<String, dynamic>()),
      ],
      verifyByDownload: j['verify_by_download'] == true,
      webLinks: j['web_links'] != false,
      hasQuota: j['quota'] is bool ? j['quota'] as bool : null,
      customDescription: j['description'] is String ? j['description'] as String : null,
      ownFolder: j['own_folder'] is bool ? j['own_folder'] as bool : null,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is CloudProvider &&
      other.id == id &&
      other.name == name &&
      other.signIn == signIn &&
      listEquals(other.fields, fields) &&
      other.verifyByDownload == verifyByDownload &&
      other.webLinks == webLinks &&
      other.hasQuota == hasQuota &&
      other.customDescription == customDescription &&
      other.ownFolder == ownFolder;

  @override
  int get hashCode => Object.hash(
    id,
    name,
    signIn,
    Object.hashAll(fields),
    verifyByDownload,
    webLinks,
    hasQuota,
    customDescription,
    ownFolder,
  );

  @override
  String toString() => 'CloudProvider($id)';
}

/// Parses `clouddrop providers --json`; throws [FormatException] when it isn't a list.
List<CloudProvider> parseProviders(String text) {
  final v = jsonDecode(text);
  if (v is! List) throw const FormatException('expected a list of providers');
  return [
    for (final p in v)
      if (p is Map && p['id'] is String) CloudProvider.fromJson(p.cast<String, dynamic>()),
  ];
}
