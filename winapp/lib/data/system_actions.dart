/// Things the app asks the operating system to do: open a folder, reveal a file in
/// Explorer, open a link, show a file or folder picker. Behind an interface so UI tests
/// can record the calls instead of launching anything.
library;

import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:path/path.dart' as p;
import 'package:url_launcher/url_launcher.dart';

abstract class SystemActions {
  /// Opens a file with its default app, or a folder in the file manager.
  Future<void> openPath(String path);

  /// Shows [path] selected in File Explorer (its folder on other systems).
  Future<void> reveal(String path);

  Future<void> openUrl(String url);

  /// Several files to add, or an empty list when cancelled.
  Future<List<String>> pickFiles({String? title});

  /// A folder, or null when cancelled.
  Future<String?> pickDirectory({String? title, String? initial});

  /// A Google client JSON file, or null when cancelled.
  Future<String?> pickJsonFile({String? initial});

  /// Where the Downloads folder is, for the key-file picker.
  String? get downloadsDir;
}

class DesktopSystemActions implements SystemActions {
  const DesktopSystemActions();

  @override
  Future<void> openPath(String path) async {
    final uri = Uri.file(path, windows: Platform.isWindows);
    await launchUrl(uri);
  }

  @override
  Future<void> reveal(String path) async {
    if (Platform.isWindows) {
      // `explorer /select,<path>` (explorer returns 1 even when it works).
      await Process.run('explorer.exe', ['/select,', path]);
      return;
    }
    final parent = FileSystemEntity.isDirectorySync(path) ? path : p.dirname(path);
    await openPath(parent);
  }

  @override
  Future<void> openUrl(String url) async {
    await launchUrl(Uri.parse(url), mode: LaunchMode.externalApplication);
  }

  @override
  Future<List<String>> pickFiles({String? title}) async {
    final files = await openFiles(confirmButtonText: 'Add');
    return [for (final f in files) f.path];
  }

  @override
  Future<String?> pickDirectory({String? title, String? initial}) =>
      getDirectoryPath(initialDirectory: initial, confirmButtonText: 'Choose');

  @override
  Future<String?> pickJsonFile({String? initial}) async {
    final f = await openFile(
      acceptedTypeGroups: const [
        XTypeGroup(label: 'Google key file (JSON)', extensions: ['json']),
      ],
      initialDirectory: initial,
    );
    return f?.path;
  }

  @override
  String? get downloadsDir {
    final home = Platform.environment[Platform.isWindows ? 'USERPROFILE' : 'HOME'];
    return home == null ? null : p.join(home, 'Downloads');
  }
}
