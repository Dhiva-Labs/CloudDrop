library;

import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/services.dart';

import '../../state/app_controller.dart';
import '../../state/setup_controller.dart';
import '../theme.dart';

/// "Copy sign-in link" that becomes active once the link is known.
class CopyLinkButton extends StatefulWidget {
  const CopyLinkButton({super.key, required this.url});

  final String? url;

  @override
  State<CopyLinkButton> createState() => _CopyLinkButtonState();
}

class _CopyLinkButtonState extends State<CopyLinkButton> {
  bool _copied = false;

  @override
  Widget build(BuildContext context) {
    final url = widget.url;
    return Button(
      onPressed: url == null
          ? null
          : () async {
              await Clipboard.setData(ClipboardData(text: url));
              if (mounted) setState(() => _copied = true);
            },
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(_copied ? FluentIcons.check_mark : FluentIcons.copy, size: 14),
          const SizedBox(width: 8),
          Text(_copied ? 'Link copied' : 'Copy sign-in link'),
        ],
      ),
    );
  }
}

/// Sign-in for an already set-up app (expired or revoked sign-in): opens Google's page
/// and waits. Returns true when signed in.
Future<bool> showSignInDialog(BuildContext context, AppController app) async {
  final flow = SignInFlow(app.engine, app.services.system);
  final started = flow.start();
  final ok = await showDialog<bool>(
    context: context,
    builder: (ctx) => _SignInDialog(flow: flow),
  );
  if (ok != true) flow.cancel();
  await started;
  flow.dispose();
  if (ok == true) {
    app.say('Signed in', Severity.success);
    await app.refreshNow();
  }
  return ok ?? false;
}

class _SignInDialog extends StatelessWidget {
  const _SignInDialog({required this.flow});

  final SignInFlow flow;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: flow,
      builder: (context, _) {
        final theme = FluentTheme.of(context);
        if (flow.phase == SignInPhase.done) {
          WidgetsBinding.instance.addPostFrameCallback((_) {
            if (context.mounted) Navigator.of(context).pop(true);
          });
        }
        final failed = flow.phase == SignInPhase.failed;
        return ContentDialog(
          title: Text(failed ? "Sign-in didn't finish" : 'Sign in to Google'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (failed)
                Text(flow.error ?? '')
              else ...[
                const Text(
                  "Your browser opened Google's sign-in page. Choose your account and "
                  'allow CloudDrop, then come back here.',
                ),
                const SizedBox(height: 16),
                Row(
                  children: [
                    const SizedBox(width: 24, height: 24, child: ProgressRing(strokeWidth: 3)),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        'Waiting for you to finish in the browser…',
                        style: theme.typography.body?.copyWith(color: theme.secondaryText),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 12),
                Row(
                  children: [
                    CopyLinkButton(url: flow.url),
                    const SizedBox(width: 8),
                    Button(
                      onPressed: flow.url == null ? null : flow.reopen,
                      child: const Text('Open again'),
                    ),
                  ],
                ),
              ],
            ],
          ),
          actions: [
            Button(
              onPressed: () => Navigator.of(context).pop(false),
              child: Text(failed ? 'Close' : 'Cancel'),
            ),
          ],
        );
      },
    );
  }
}
