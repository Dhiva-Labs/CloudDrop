/// Copying files into the CloudDrop folder, and folder-path helpers
/// (the Dart twin of `gui/src/fsops.rs`).
library;

import 'dart:io';
import 'dart:isolate';

import 'package:path/path.dart' as p;

import 'models.dart';

/// "report.pdf" -> "report (2).pdf"; ".bashrc" -> ".bashrc (2)" (`util::numbered_name`).
String numberedName(String name, int n) {
  final i = name.lastIndexOf('.');
  if (i > 0) return '${name.substring(0, i)} ($n)${name.substring(i)}';
  return '$name ($n)';
}

/// The first of "name", "name (2)", "name (3)", ... for which [taken] is false.
String uniqueName(String name, bool Function(String candidate) taken) {
  if (!taken(name)) return name;
  for (var n = 2; ; n++) {
    final c = numberedName(name, n);
    if (!taken(c)) return c;
  }
}

/// `rel` equals or lies beneath one of [list].
bool covered(List<String> list, String rel) => list.any((k) {
  final key = _trimSlashes(k);
  return rel == key || rel.startsWith('$key/');
});

String _trimSlashes(String s) {
  var a = 0;
  var b = s.length;
  while (a < b && s[a] == '/') {
    a++;
  }
  while (b > a && s[b - 1] == '/') {
    b--;
  }
  return s.substring(a, b);
}

/// Adds a keep-local folder, dropping entries it makes redundant. null if already covered.
List<String>? addKeep(List<String> list, String rel) {
  final r = _trimSlashes(rel);
  if (r.isEmpty || covered(list, r)) return null;
  final out = [
    for (final k in list)
      if (!covered([r], _trimSlashes(k))) k,
    r,
  ]..sort();
  return out;
}

/// `picked` as a path relative to [syncDir] ('/' separated), if it is a folder beneath it.
String? relInSync(String syncDir, String picked, {p.Context? context}) {
  final c = context ?? p.context;
  final root = c.normalize(syncDir);
  final target = c.normalize(picked);
  if (!c.isWithin(root, target)) return null;
  final rel = c.split(c.relative(target, from: root)).join('/');
  return rel.isEmpty ? null : rel;
}

class CopyReport {
  CopyReport({this.items = 0, this.files = 0, this.bytes = 0, List<String>? problems})
    : problems = problems ?? [];

  /// Top-level items copied.
  int items;
  int files;
  int bytes;
  final List<String> problems;

  String summary(String destName) {
    final what = items == 1 ? '1 item' : '$items items';
    var s = items == 0
        ? 'Nothing was copied.'
        : 'Copied $what to $destName. Uploading will start shortly.';
    if (problems.isNotEmpty) {
      s += " ${problems.length} couldn't be copied (${problems.first}).";
    }
    return s;
  }
}

/// Copies [sources] into [destDir] without ever replacing anything: a taken name gets
/// " (2)", " (3)", ... [reservedNames] holds names that exist only in Drive (cloud-only
/// entries of the destination folder). Files appear atomically: copied under a hidden
/// `.clouddrop-copy-….part` name (ignored by the engine), then renamed.
CopyReport copyInto(
  String destDir,
  List<String> sources, {
  Set<String> reservedNames = const {},
  bool caseInsensitive = false,
}) {
  final report = CopyReport();
  String key(String s) => caseInsensitive ? s.toLowerCase() : s;
  final reserved = {for (final n in reservedNames) key(n)};
  try {
    Directory(destDir).createSync(recursive: true);
  } on FileSystemException catch (e) {
    report.problems.add('$destDir: ${e.osError?.message ?? e.message}');
    return report;
  }
  for (final src in sources) {
    final name = p.basename(src);
    if (name.isEmpty) continue;
    if (p.equals(src, destDir) || p.isWithin(src, destDir)) {
      report.problems.add("$name: a folder can't be copied into itself");
      continue;
    }
    final unique = uniqueName(name, (c) {
      if (reserved.contains(key(c))) return true;
      return FileSystemEntity.typeSync(p.join(destDir, c), followLinks: false) !=
          FileSystemEntityType.notFound;
    });
    reserved.add(key(unique));
    try {
      if (_copyTree(src, p.join(destDir, unique), report)) report.items++;
    } on FileSystemException catch (e) {
      report.problems.add('$name: ${e.osError?.message ?? e.message}');
    }
  }
  return report;
}

/// [copyInto] on a background isolate, so big copies never freeze the window.
Future<CopyReport> copyIntoAsync(
  String destDir,
  List<String> sources, {
  Set<String> reservedNames = const {},
  bool caseInsensitive = false,
}) => Isolate.run(
  () => copyInto(destDir, sources, reservedNames: reservedNames, caseInsensitive: caseInsensitive),
);

/// Returns false for things that are skipped (symlinks, sockets, ...).
bool _copyTree(String src, String dest, CopyReport report) {
  final type = FileSystemEntity.typeSync(src, followLinks: false);
  switch (type) {
    case FileSystemEntityType.directory:
      Directory(dest).createSync();
      for (final e in Directory(src).listSync(followLinks: false)) {
        final name = p.basename(e.path);
        try {
          _copyTree(e.path, p.join(dest, name), report);
        } on FileSystemException catch (err) {
          report.problems.add('$name: ${err.osError?.message ?? err.message}');
        }
      }
      return true;
    case FileSystemEntityType.file:
      final name = p.basename(dest);
      final tmp = p.join(p.dirname(dest), '.clouddrop-copy-$pid-$name.part');
      try {
        final f = File(src);
        final modified = f.lastModifiedSync();
        final copied = f.copySync(tmp);
        copied.setLastModifiedSync(modified);
        final size = copied.lengthSync();
        copied.renameSync(dest);
        report.files++;
        report.bytes += size;
        return true;
      } on FileSystemException {
        try {
          File(tmp).deleteSync();
        } catch (_) {}
        rethrow;
      }
    default:
      return false;
  }
}

/// Entry paths (relative, '/' separated) of everything directly inside [dir] that exists
/// only in Drive or anywhere in the database, to keep copies from reusing those names.
Set<String> reservedFor(Iterable<DirItem> itemsInDir) => {for (final i in itemsInDir) i.name};
