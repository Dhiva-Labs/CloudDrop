/// First-run assistant: Welcome -> Google key -> Folders -> Sign in -> Done
/// (`gui/src/setup.rs`).
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../state/setup_controller.dart';
import '../theme.dart';
import '../widgets/common.dart';
import '../widgets/sign_in_dialog.dart';

class SetupPage extends StatelessWidget {
  const SetupPage({super.key, required this.controller});

  final SetupController controller;

  @override
  Widget build(BuildContext context) {
    return ScaffoldPage(
      padding: EdgeInsets.zero,
      content: ListenableBuilder(
        listenable: controller,
        builder: (context, _) {
          final step = controller.step;
          return LayoutBuilder(
            builder: (context, c) {
              final gutter = pageGutter(c.maxWidth);
              return Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _StepHeader(controller: controller, gutter: gutter),
                  Expanded(
                    child: SingleChildScrollView(
                      padding: EdgeInsets.fromLTRB(gutter, 8, gutter, 24),
                      child: Align(
                        alignment: Alignment.topCenter,
                        child: ConstrainedBox(
                          constraints: const BoxConstraints(maxWidth: 640),
                          child: switch (step) {
                            SetupStep.welcome => _Welcome(controller: controller),
                            SetupStep.key => _KeyPage(controller: controller),
                            SetupStep.folder => _FolderPage(controller: controller),
                            SetupStep.signIn => _SignInPage(controller: controller),
                            SetupStep.done => _DonePage(controller: controller),
                          },
                        ),
                      ),
                    ),
                  ),
                ],
              );
            },
          );
        },
      ),
    );
  }
}

class _StepHeader extends StatelessWidget {
  const _StepHeader({required this.controller, required this.gutter});

  final SetupController controller;
  final double gutter;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final n = controller.stepCount;
    final i = controller.index;
    final label = switch (controller.step) {
      SetupStep.welcome => 'Welcome',
      SetupStep.key => 'Google key',
      SetupStep.folder => 'Folders',
      SetupStep.signIn => 'Sign in',
      SetupStep.done => 'Done',
    };
    return Padding(
      padding: EdgeInsets.fromLTRB(gutter, 16, gutter, 8),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 640),
          child: Column(
            children: [
              Row(
                children: [
                  if (controller.canGoBack)
                    Tooltip(
                      message: 'Back',
                      child: IconButton(
                        icon: const Icon(FluentIcons.back, size: 14),
                        onPressed: controller.back,
                      ),
                    )
                  else
                    const SizedBox(width: 32),
                  const SizedBox(width: 8),
                  Expanded(child: Text('Set up CloudDrop', style: theme.typography.bodyStrong)),
                  Text(
                    'Step ${i + 1} of $n · $label',
                    style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              // One segment per step, filled up to the current one.
              Row(
                children: [
                  for (var k = 0; k < n; k++) ...[
                    if (k > 0) const SizedBox(width: 4),
                    Expanded(
                      child: Container(
                        height: 3,
                        decoration: BoxDecoration(
                          borderRadius: BorderRadius.circular(2),
                          color: k <= i
                              ? theme.accentColor.defaultBrushFor(theme.brightness)
                              : theme.resources.controlStrokeColorDefault,
                        ),
                      ),
                    ),
                  ],
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

Widget _title(BuildContext context, String text, {String? subtitle, bool center = false}) {
  final theme = FluentTheme.of(context);
  return Column(
    crossAxisAlignment: center ? CrossAxisAlignment.center : CrossAxisAlignment.start,
    children: [
      Text(text, style: theme.typography.title, textAlign: center ? TextAlign.center : null),
      if (subtitle != null) ...[
        const SizedBox(height: 8),
        Text(
          subtitle,
          textAlign: center ? TextAlign.center : null,
          style: theme.typography.body?.copyWith(color: theme.secondaryText),
        ),
      ],
    ],
  );
}

class _Logo extends StatelessWidget {
  const _Logo(this.size);
  final double size;

  @override
  Widget build(BuildContext context) => Image.asset(
    'assets/icon.png',
    width: size,
    height: size,
    filterQuality: FilterQuality.medium,
    errorBuilder: (_, _, _) => Icon(FluentIcons.cloud_upload, size: size * 0.7),
  );
}

// ---------- welcome ----------

class _Welcome extends StatelessWidget {
  const _Welcome({required this.controller});

  final SetupController controller;

  @override
  Widget build(BuildContext context) {
    const points = [
      (
        FluentIcons.fabric_folder,
        'Keeps your folders',
        'The same folder structure appears in My Drive.',
      ),
      (FluentIcons.sync, 'Uploads by itself', 'New and changed files go up in the background.'),
      (FluentIcons.shield, 'Never deletes in Drive', 'Free up space here without losing anything.'),
    ];
    final theme = FluentTheme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        // The logo on a soft glow of its own sky.
        SizedBox(
          height: 150,
          child: Stack(
            alignment: Alignment.center,
            children: [
              Container(
                width: 190,
                height: 150,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  gradient: RadialGradient(
                    colors: [
                      Brand.blue.withValues(alpha: theme.isDark ? 0.30 : 0.22),
                      Brand.blue.withValues(alpha: 0),
                    ],
                  ),
                ),
              ),
              const _Logo(104),
            ],
          ),
        ),
        const SizedBox(height: 8),
        _title(
          context,
          'Welcome to CloudDrop',
          subtitle:
              'Anything you put in your CloudDrop folder is uploaded to Google Drive. '
              'Remove it from this computer whenever you like — it stays in Drive.',
          center: true,
        ),
        const SizedBox(height: 24),
        Surface(
          padding: const EdgeInsets.symmetric(vertical: 4),
          child: Column(
            children: [
              for (var k = 0; k < points.length; k++) ...[
                if (k > 0)
                  const Padding(padding: EdgeInsets.symmetric(horizontal: 12), child: Divider()),
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                  child: Row(
                    children: [
                      IconBadge(icon: points[k].$1, tint: theme.accent, size: 36),
                      const SizedBox(width: 14),
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(points[k].$2, style: theme.typography.bodyStrong),
                            Text(
                              points[k].$3,
                              style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                            ),
                          ],
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ],
          ),
        ),
        const SizedBox(height: 16),
        Center(
          child: FilledButton(
            onPressed: controller.next,
            child: const Padding(
              padding: EdgeInsets.symmetric(horizontal: 24, vertical: 2),
              child: Text('Get Started'),
            ),
          ),
        ),
      ],
    );
  }
}

// ---------- key ----------

class _KeyPage extends StatefulWidget {
  const _KeyPage({required this.controller});

  final SetupController controller;

  @override
  State<_KeyPage> createState() => _KeyPageState();
}

class _KeyPageState extends State<_KeyPage> {
  late final TextEditingController _id;
  late final TextEditingController _secret;

  SetupController get c => widget.controller;

  @override
  void initState() {
    super.initState();
    _id = TextEditingController(text: c.clientId);
    _secret = TextEditingController(text: c.clientSecret);
    c.addListener(_sync);
  }

  void _sync() {
    // A chosen key file (or a pasted JSON) fills the fields.
    if (_id.text != c.clientId) _id.text = c.clientId;
    if (_secret.text != c.clientSecret) _secret.text = c.clientSecret;
  }

  @override
  void dispose() {
    c.removeListener(_sync);
    _id.dispose();
    _secret.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final app = c.app;
    return ListenableBuilder(
      listenable: c,
      builder: (context, _) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _title(
            context,
            'Connect a Google key',
            subtitle:
                'Google asks every app that uploads to Drive to use a key from your own '
                'Google account. It takes about five minutes, once, and the key only lets '
                'CloudDrop see the files it uploads.',
          ),
          const SectionTitle('Create your key'),
          Text(
            'Each button opens the right page in Google Cloud.',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
          const SizedBox(height: 8),
          Surface(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Column(
              children: [
                for (var i = 0; i < googleKeySteps.length; i++) ...[
                  if (i > 0)
                    const Padding(padding: EdgeInsets.symmetric(horizontal: 12), child: Divider()),
                  Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                    child: Row(
                      children: [
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(googleKeySteps[i].title),
                              const SizedBox(height: 2),
                              Text(
                                googleKeySteps[i].text,
                                style: theme.typography.caption?.copyWith(
                                  color: theme.secondaryText,
                                ),
                              ),
                            ],
                          ),
                        ),
                        const SizedBox(width: 12),
                        Button(
                          onPressed: () => app.services.system.openUrl(googleKeySteps[i].url),
                          child: const Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              Text('Open'),
                              SizedBox(width: 6),
                              Icon(FluentIcons.open_in_new_window, size: 11),
                            ],
                          ),
                        ),
                      ],
                    ),
                  ),
                ],
              ],
            ),
          ),
          const SectionTitle('Add your key'),
          Text(
            'Choose the JSON file you downloaded, or paste the two values.',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
          const SizedBox(height: 10),
          Align(
            alignment: Alignment.centerLeft,
            child: Button(
              onPressed: c.chooseKeyFile,
              child: const Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(FluentIcons.open_file, size: 13),
                  SizedBox(width: 8),
                  Text('Choose downloaded key file…'),
                ],
              ),
            ),
          ),
          const SizedBox(height: 12),
          InfoLabel(
            label: 'Client ID',
            child: TextBox(
              controller: _id,
              placeholder: '123456-abc.apps.googleusercontent.com',
              onChanged: c.setClientId,
            ),
          ),
          const SizedBox(height: 10),
          InfoLabel(
            label: 'Client secret',
            child: PasswordBox(
              controller: _secret,
              placeholder: c.hasStoredSecret ? 'Saved — leave empty to keep it' : 'GOCSPX-…',
              onChanged: c.setClientSecret,
            ),
          ),
          if (c.keyError != null) ...[
            const SizedBox(height: 12),
            InfoBar(title: Text(c.keyError!), severity: InfoBarSeverity.error),
          ],
          const SizedBox(height: 20),
          Align(
            alignment: Alignment.centerRight,
            child: FilledButton(
              onPressed: c.keyValid ? c.next : null,
              child: const Text('Continue'),
            ),
          ),
        ],
      ),
    );
  }
}

// ---------- folders ----------

class _FolderPage extends StatefulWidget {
  const _FolderPage({required this.controller});

  final SetupController controller;

  @override
  State<_FolderPage> createState() => _FolderPageState();
}

class _FolderPageState extends State<_FolderPage> {
  late final TextEditingController _drive;

  SetupController get c => widget.controller;

  @override
  void initState() {
    super.initState();
    _drive = TextEditingController(text: c.driveFolder);
  }

  @override
  void dispose() {
    _drive.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return ListenableBuilder(
      listenable: c,
      builder: (context, _) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _title(context, 'Choose your folders'),
          const SectionTitle('On this computer'),
          Surface(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
            child: Row(
              children: [
                const FileTypeIcon(name: '', isDir: true, size: 26),
                const SizedBox(width: 14),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Text('CloudDrop folder'),
                      const SizedBox(height: 2),
                      Text(
                        c.app.paths.pretty(c.folder),
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                      ),
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                Tooltip(
                  message: c.folderLocked
                      ? "The folder can't be changed after CloudDrop has used it"
                      : 'Choose another folder',
                  child: Button(
                    onPressed: c.folderLocked ? null : c.chooseFolder,
                    child: const Text('Change…'),
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(height: 6),
          Text(
            'It appears in File Explorer\'s sidebar, so you can open it like any other folder.',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
          const SectionTitle('In Google Drive'),
          Text(
            'Your files appear in this folder in My Drive, with the same subfolders.',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
          const SizedBox(height: 8),
          InfoLabel(
            label: 'Folder name in My Drive',
            child: TextBox(controller: _drive, onChanged: c.setDriveFolder),
          ),
          const SizedBox(height: 24),
          Align(
            alignment: Alignment.centerRight,
            child: FilledButton(
              onPressed: c.folderValid ? c.next : null,
              child: const Text('Continue'),
            ),
          ),
        ],
      ),
    );
  }
}

// ---------- sign in ----------

class _SignInPage extends StatelessWidget {
  const _SignInPage({required this.controller});

  final SetupController controller;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final c = controller;
    final flow = c.signIn;
    final working = flow.active || c.settingUp;
    final error =
        c.setupError ??
        (flow.phase == SignInPhase.failed ? "Sign-in didn't finish: ${flow.error}" : null);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 8),
        const Center(child: _Logo(88)),
        const SizedBox(height: 16),
        _title(
          context,
          'Sign in to Google',
          subtitle:
              "Your browser will open Google's sign-in page. Choose your account and "
              'allow CloudDrop to add files to your Drive.',
          center: true,
        ),
        if (!c.app.hasBuiltinClient) ...[
          const SizedBox(height: 10),
          Text(
            "If Google says the app isn't verified, that's expected for your own key: "
            'choose Advanced → Go to (your app name).',
            textAlign: TextAlign.center,
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
        ],
        const SizedBox(height: 24),
        if (flow.active) ...[
          const Center(child: ProgressRing()),
          const SizedBox(height: 12),
          Text(
            'Waiting for you to finish in the browser…',
            textAlign: TextAlign.center,
            style: theme.typography.body?.copyWith(color: theme.secondaryText),
          ),
          const SizedBox(height: 12),
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              CopyLinkButton(url: flow.url),
              const SizedBox(width: 8),
              Button(
                onPressed: flow.url == null ? null : flow.reopen,
                child: const Text('Open again'),
              ),
            ],
          ),
        ] else if (c.settingUp) ...[
          const Center(child: ProgressRing()),
        ],
        if (error != null) ...[
          const SizedBox(height: 12),
          InfoBar(title: Text(error), severity: InfoBarSeverity.error),
        ],
        const SizedBox(height: 16),
        if (!working)
          Center(
            child: FilledButton(
              onPressed: c.signInWithGoogle,
              child: const Padding(
                padding: EdgeInsets.symmetric(horizontal: 16, vertical: 2),
                child: Text('Sign In with Google'),
              ),
            ),
          ),
        if (!working && c.alreadySignedIn) ...[
          const SizedBox(height: 8),
          Center(
            child: HyperlinkButton(
              onPressed: c.keepCurrentSignIn,
              child: const Text('Keep Current Sign-in'),
            ),
          ),
        ],
        if (flow.active)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Center(
              child: Button(onPressed: flow.cancel, child: const Text('Cancel')),
            ),
          ),
      ],
    );
  }
}

// ---------- done ----------

class _DonePage extends StatelessWidget {
  const _DonePage({required this.controller});

  final SetupController controller;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final c = controller;
    final (title, sub, busy) = switch (c.serviceState) {
      ServiceStart.starting => (
        'Starting CloudDrop…',
        'It runs in the background and starts when you sign in.',
        true,
      ),
      ServiceStart.running => (
        'CloudDrop is running',
        c.serviceError ?? 'It runs in the background and starts when you sign in.',
        false,
      ),
      ServiceStart.failed => ("CloudDrop couldn't start", c.serviceError ?? '', false),
    };
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 8),
        Center(
          child: IconBadge(icon: FluentIcons.completed_solid, tint: theme.success, size: 88),
        ),
        const SizedBox(height: 16),
        _title(
          context,
          "You're all set",
          subtitle:
              'Drop files into ${c.app.paths.pretty(c.folder)} and '
              "they'll show up in My Drive → ${c.driveFolder.trim()}.",
          center: true,
        ),
        const SizedBox(height: 24),
        Surface(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
          child: Row(
            children: [
              if (busy)
                const SizedBox(width: 22, height: 22, child: ProgressRing(strokeWidth: 3))
              else
                Icon(
                  c.serviceState == ServiceStart.failed
                      ? FluentIcons.warning
                      : FluentIcons.completed_solid,
                  size: 22,
                  color: c.serviceState == ServiceStart.failed
                      ? theme.resources.systemFillColorCaution
                      : theme.resources.systemFillColorSuccess,
                ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title),
                    if (sub.isNotEmpty)
                      Text(
                        sub,
                        style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                      ),
                  ],
                ),
              ),
              if (c.serviceState == ServiceStart.failed)
                Button(onPressed: c.startService, child: const Text('Try Again')),
            ],
          ),
        ),
        const SizedBox(height: 8),
        Surface(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
          child: Row(
            children: [
              Icon(FluentIcons.fabric_folder_fill, size: 22, color: const Color(0xFFF2B63C)),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text('CloudDrop is in File Explorer'),
                    Text(
                      "Find it in File Explorer's sidebar. Files show a cloud icon until they're "
                      'downloaded, and a green check when they are on this computer.',
                      style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
        const SizedBox(height: 20),
        Center(
          child: Button(onPressed: c.openFolder, child: const Text('Open CloudDrop Folder')),
        ),
        const SizedBox(height: 12),
        Center(
          child: FilledButton(
            onPressed: c.finish,
            child: const Padding(
              padding: EdgeInsets.symmetric(horizontal: 32, vertical: 2),
              child: Text('Finish'),
            ),
          ),
        ),
      ],
    );
  }
}
