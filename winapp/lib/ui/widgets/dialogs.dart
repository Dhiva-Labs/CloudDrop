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

/// "Sign out?" - true when the user confirms.
Future<bool> confirmSignOut(BuildContext context) async {
  final r = await showDialog<bool>(
    context: context,
    builder: (ctx) => ContentDialog(
      title: const Text('Sign out?'),
      content: const Text(
        'Uploads stop until you sign in again. Nothing is removed from Google Drive or this '
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
