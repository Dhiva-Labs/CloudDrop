library;

import 'package:fluent_ui/fluent_ui.dart';

/// "Free up space?" - true when the user confirms.
Future<bool> confirmFree(BuildContext context, {required String body}) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: const Text('Free up space?'),
      content: Text(body),
      actions: [
        Button(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
        FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Free Up Space')),
      ],
    ),
  );
  return r ?? false;
}

/// "Sign out?" - true when the user confirms. [cloud] names where CloudDrop uploads.
Future<bool> confirmSignOut(BuildContext context, {String cloud = 'Google Drive'}) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: const Text('Sign out?'),
      content: Text(
        'Uploads stop until you sign in again. Nothing is removed from $cloud or this '
        'computer.',
      ),
      actions: [
        Button(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
        FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Sign Out')),
      ],
    ),
  );
  return r ?? false;
}

/// "Switch to another cloud?" - true when the user wants to choose one. [folder] names the
/// folder that moves (with several).
Future<bool> confirmSwitchProvider(
  BuildContext context, {
  required String cloud,
  String folder = 'your CloudDrop folder',
}) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: const Text('Switch to another cloud?'),
      content: Text(
        'CloudDrop will upload everything in $folder again, to the cloud you '
        'choose next. Nothing is deleted from $cloud. Files that are only in $cloud (freed '
        'from this computer) stay there, but CloudDrop stops listing them.',
      ),
      actions: [
        Button(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
        FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Choose Cloud…')),
      ],
    ),
  );
  return r ?? false;
}

/// "Remove this folder?" (Settings › Remove This Folder…) - true when the user confirms.
/// [folder] is the folder as shown, [cloud] where it uploads; [last]: it is the only one.
Future<bool> confirmRemoveFolder(
  BuildContext context, {
  required String folder,
  required String cloud,
  bool last = false,
}) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: const Text('Remove this folder?'),
      content: Text(
        'CloudDrop stops uploading $folder and forgets what it uploaded from it. Nothing is '
        'deleted: the folder keeps its files, and $cloud keeps everything uploaded so far, '
        'including the files freed from this computer.'
        '${last ? '\n\nIt is your only folder, so CloudDrop asks you to set one up next.' : ''}',
      ),
      actions: [
        Button(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
        FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Remove Folder')),
      ],
    ),
  );
  return r ?? false;
}

/// A simple message with an OK button.
Future<void> showNotice(BuildContext context, {required String title, required String body}) {
  return showDialog<void>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: Text(title),
      content: Text(body),
      actions: [FilledButton(onPressed: () => Navigator.pop(ctx), child: const Text('OK'))],
    ),
  );
}
