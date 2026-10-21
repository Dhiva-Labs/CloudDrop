/// First-run assistant: Welcome -> Upload to -> Google key -> Folders -> Sign in (or Connect)
/// -> Done (`gui/src/setup.rs`).
library;

import 'package:fluent_ui/fluent_ui.dart';
import 'package:flutter/services.dart';

import '../../data/providers.dart';
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
                            SetupStep.provider => _ProviderPage(controller: controller),
                            SetupStep.key => _KeyPage(controller: controller),
                            SetupStep.folder => _FolderPage(controller: controller),
                            SetupStep.signIn =>
                              controller.chosen.signIn == SignInKind.form
                                  // A page per provider, so its fields start from its own values.
                                  ? _ConnectPage(
                                      key: ValueKey(controller.chosen.id),
                                      controller: controller,
                                    )
                                  : _SignInPage(
                                      key: ValueKey(controller.chosen.id),
                                      controller: controller,
                                    ),
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
    final label = controller.stepLabel;
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
                  Expanded(child: Text(controller.title, style: theme.typography.bodyStrong)),
                  Text(
                    'Step ${i + 1} of $n · $label',
                    style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                  ),
                  if (controller.canCancel) ...[
                    const SizedBox(width: 12),
                    Button(onPressed: controller.cancel, child: const Text('Cancel')),
                  ],
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
        'The same folder structure appears in the cloud.',
      ),
      (FluentIcons.sync, 'Uploads by itself', 'New and changed files go up in the background.'),
      (
        FluentIcons.shield,
        'Never deletes in the cloud',
        'Free up space here without losing anything.',
      ),
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
              'Anything you put in your CloudDrop folder is uploaded to the cloud. '
              'Remove it from this computer whenever you like — it stays there.',
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

// ---------- where to upload ----------

class _ProviderPage extends StatelessWidget {
  const _ProviderPage({required this.controller});

  final SetupController controller;

  @override
  Widget build(BuildContext context) {
    final c = controller;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title(
          context,
          'Choose where to upload',
          subtitle: c.switching
              ? 'Everything in your CloudDrop folder is uploaded again to the place you pick. '
                    'Nothing is deleted from ${c.current.name}.'
              : c.adding
              ? 'Pick the cloud the new folder uploads to. It can be another one than your '
                    'other folders use.'
              : 'Pick the cloud CloudDrop uploads to. You can switch later in Settings.',
        ),
        const SizedBox(height: 20),
        RadioGroup<String>(
          groupValue: c.chosen.id,
          onChanged: (id) {
            for (final p in c.providers) {
              if (p.id == id) c.choose(p);
            }
          },
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (final p in c.providers) ...[
                _ProviderCard(
                  provider: p,
                  selected: p.id == c.chosen.id,
                  onPressed: () => c.choose(p),
                ),
                const SizedBox(height: 8),
              ],
            ],
          ),
        ),
        const SizedBox(height: 12),
        Align(
          alignment: Alignment.centerRight,
          child: FilledButton(onPressed: c.next, child: const Text('Continue')),
        ),
      ],
    );
  }
}

class _ProviderCard extends StatelessWidget {
  const _ProviderCard({required this.provider, required this.selected, required this.onPressed});

  final CloudProvider provider;
  final bool selected;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final accent = theme.accentColor.defaultBrushFor(theme.brightness);
    final p = provider;
    return Semantics(
      selected: selected,
      button: true,
      label: p.name,
      child: HoverButton(
        onPressed: onPressed,
        cursor: SystemMouseCursors.click,
        builder: (context, states) {
          final hovered = states.isHovered || states.isFocused;
          return Container(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
            decoration: BoxDecoration(
              color: hovered && !selected
                  ? theme.resources.cardBackgroundFillColorSecondary
                  : theme.cardFill,
              borderRadius: BorderRadius.circular(8),
              border: Border.all(
                color: selected ? accent : theme.cardStroke,
                width: selected ? 2 : 1,
              ),
            ),
            child: Row(
              children: [
                IconBadge(icon: providerIcon(p.id), tint: theme.accent, size: 40),
                const SizedBox(width: 14),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(p.name, style: theme.typography.bodyStrong),
                      if (p.description.isNotEmpty)
                        Text(
                          p.description,
                          style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                        ),
                      if (p.ownFolderOnly) ...[
                        const SizedBox(height: 4),
                        Row(
                          children: [
                            Icon(FluentIcons.shield, size: 12, color: theme.success.fg),
                            const SizedBox(width: 6),
                            Flexible(
                              child: Text(
                                'CloudDrop only sees its own folder',
                                style: theme.typography.caption,
                              ),
                            ),
                          ],
                        ),
                      ],
                    ],
                  ),
                ),
                const SizedBox(width: 12),
                RadioButton<String>(value: p.id, semanticLabel: p.name),
              ],
            ),
          );
        },
      ),
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
    c.addListener(_sync);
  }

  /// A new folder's cloud folder takes the folder's name until the user types one.
  void _sync() {
    if (_drive.text != c.driveFolder) _drive.text = c.driveFolder;
  }

  @override
  void dispose() {
    c.removeListener(_sync);
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
          _title(
            context,
            'Choose your folders',
            subtitle: c.adding
                ? 'Pick a folder no other CloudDrop folder holds or is inside of.'
                : null,
          ),
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
                      Text(c.adding ? 'Folder to upload' : 'CloudDrop folder'),
                      const SizedBox(height: 2),
                      Text(
                        c.folder.isEmpty ? 'No folder chosen yet' : c.app.paths.pretty(c.folder),
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
                  child: c.adding && c.folder.isEmpty
                      ? FilledButton(onPressed: c.chooseFolder, child: const Text('Choose…'))
                      : Button(
                          onPressed: c.folderLocked ? null : c.chooseFolder,
                          child: const Text('Change…'),
                        ),
                ),
              ],
            ),
          ),
          const SizedBox(height: 6),
          if (c.folderProblem != null)
            InfoBar(title: Text(c.folderProblem!), severity: InfoBarSeverity.warning)
          else
            Text(
              c.adding && c.folder.isNotEmpty
                  ? 'CloudDrop calls it “${c.newConnectionName}”. It appears in File '
                        "Explorer's sidebar, so you can open it like any other folder."
                  : 'It appears in File Explorer\'s sidebar, so you can open it like any other folder.',
              style: theme.typography.caption?.copyWith(color: theme.secondaryText),
            ),
          SectionTitle('In ${c.chosen.name}'),
          Text(
            'Your files appear in this folder in ${c.chosen.rootLabel}, with the same subfolders.',
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
          const SizedBox(height: 8),
          InfoLabel(
            label: 'Folder name in ${c.chosen.rootLabel}',
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

class _SignInPage extends StatefulWidget {
  const _SignInPage({super.key, required this.controller});

  final SetupController controller;

  @override
  State<_SignInPage> createState() => _SignInPageState();
}

class _SignInPageState extends State<_SignInPage> {
  late final TextEditingController _server;

  SetupController get c => widget.controller;

  @override
  void initState() {
    super.initState();
    _server = TextEditingController(text: c.serverUrl);
  }

  @override
  void dispose() {
    _server.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final p = c.chosen;
    final flow = c.signIn;
    final working = flow.active || c.settingUp;
    final nextcloud = p.signIn == SignInKind.nextcloud;
    final error =
        c.setupError ??
        (flow.phase == SignInPhase.failed ? "Sign-in didn't finish: ${flow.error}" : null);
    final (title, text) = switch (p.signIn) {
      SignInKind.google => (
        'Sign in to Google',
        "Your browser will open Google's sign-in page. Choose your account and "
            'allow CloudDrop to add files to your Drive.',
      ),
      SignInKind.nextcloud => (
        'Sign in to ${p.name}',
        "Enter your server's address. Your browser opens it so you can log in and allow "
            'CloudDrop, which gets an app password of its own — never your password.',
      ),
      SignInKind.oauth || SignInKind.form => (
        'Sign in to ${p.name}',
        "Your browser will open ${p.name}'s sign-in page. Choose your account and "
            'allow CloudDrop to add files to its folder.',
      ),
    };
    final serverProblem = c.serverUrl.trim().isNotEmpty && !c.serverValid;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SizedBox(height: 8),
        const Center(child: _Logo(88)),
        const SizedBox(height: 16),
        _title(context, title, subtitle: text, center: true),
        if (p.isGoogle && !c.app.hasBuiltinClient) ...[
          const SizedBox(height: 10),
          Text(
            "If Google says the app isn't verified, that's expected for your own key: "
            'choose Advanced → Go to (your app name).',
            textAlign: TextAlign.center,
            style: theme.typography.caption?.copyWith(color: theme.secondaryText),
          ),
        ],
        if (nextcloud) ...[
          const SizedBox(height: 20),
          InfoLabel(
            label: 'Server address',
            child: TextBox(
              controller: _server,
              placeholder: 'https://cloud.example.com',
              keyboardType: TextInputType.url,
              enabled: !working,
              onChanged: c.setServerUrl,
            ),
          ),
          if (serverProblem) ...[
            const SizedBox(height: 4),
            Text(
              'Enter an address that starts with https://',
              style: theme.typography.caption?.copyWith(
                color: theme.resources.systemFillColorCritical,
              ),
            ),
          ],
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
              onPressed: nextcloud && !c.serverValid ? null : c.signInWithBrowser,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 2),
                child: Text(
                  p.isGoogle
                      ? 'Sign In with Google'
                      : nextcloud
                      ? 'Sign In'
                      : 'Sign In with ${p.name}',
                ),
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

// ---------- connect (a form: WebDAV, S3, SFTP, MEGA) ----------

class _ConnectPage extends StatefulWidget {
  const _ConnectPage({super.key, required this.controller});

  final SetupController controller;

  @override
  State<_ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<_ConnectPage> {
  final Map<String, TextEditingController> _boxes = {};

  SetupController get c => widget.controller;

  @override
  void initState() {
    super.initState();
    for (final f in c.chosen.fields) {
      _boxes[f.key] = TextEditingController(text: c.fieldValue(f.key));
    }
    c.addListener(_sync);
  }

  void _sync() {
    // A chosen file fills its field.
    for (final e in _boxes.entries) {
      final v = c.fieldValue(e.key);
      if (e.value.text != v) e.value.text = v;
    }
  }

  @override
  void dispose() {
    c.removeListener(_sync);
    for (final b in _boxes.values) {
      b.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final p = c.chosen;
    final testing = c.test == ConnectionTest.testing;
    final busy = testing || c.settingUp;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title(
          context,
          'Connect to ${p.name}',
          subtitle:
              'Enter your ${p.name} details. Passwords and keys are saved apart from the '
              'settings, readable only by you.',
        ),
        const SizedBox(height: 16),
        for (final f in p.fields) ...[_field(context, f, busy), const SizedBox(height: 12)],
        if (testing)
          Row(
            children: [
              const SizedBox(width: 20, height: 20, child: ProgressRing(strokeWidth: 3)),
              const SizedBox(width: 12),
              Text(
                'Testing the connection…',
                style: theme.typography.body?.copyWith(color: theme.secondaryText),
              ),
            ],
          ),
        if (c.test == ConnectionTest.ok)
          InfoBar(title: Text(c.testMessage ?? ''), severity: InfoBarSeverity.success),
        if (c.test == ConnectionTest.failed)
          InfoBar(
            title: Text("Couldn't connect: ${c.testMessage ?? ''}"),
            severity: InfoBarSeverity.error,
          ),
        if (c.setupError != null) ...[
          const SizedBox(height: 8),
          InfoBar(title: Text(c.setupError!), severity: InfoBarSeverity.error),
        ],
        const SizedBox(height: 20),
        Row(
          mainAxisAlignment: MainAxisAlignment.end,
          children: [
            Button(
              onPressed: c.formValid && !busy ? c.testConnection : null,
              child: const Text('Test Connection'),
            ),
            const SizedBox(width: 8),
            FilledButton(
              onPressed: c.test == ConnectionTest.ok && !busy ? c.continueConnected : null,
              child: const Text('Continue'),
            ),
          ],
        ),
      ],
    );
  }

  Widget _field(BuildContext context, ProviderField f, bool busy) {
    final theme = FluentTheme.of(context);
    final box = _boxes[f.key]!;
    final value = c.fieldValue(f.key);
    final problem = value.trim().isEmpty ? null : f.problem(value);
    final Widget input = switch (f.kind) {
      FieldKind.secret => PasswordBox(
        controller: box,
        enabled: !busy,
        onChanged: (v) => c.setField(f.key, v),
      ),
      FieldKind.file => Row(
        children: [
          Expanded(
            child: TextBox(controller: box, enabled: !busy, onChanged: (v) => c.setField(f.key, v)),
          ),
          const SizedBox(width: 8),
          Button(onPressed: busy ? null : () => c.chooseFieldFile(f), child: const Text('Choose…')),
        ],
      ),
      FieldKind.number => TextBox(
        controller: box,
        enabled: !busy,
        keyboardType: TextInputType.number,
        inputFormatters: [FilteringTextInputFormatter.digitsOnly],
        onChanged: (v) => c.setField(f.key, v),
      ),
      FieldKind.url => TextBox(
        controller: box,
        enabled: !busy,
        placeholder: 'https://',
        keyboardType: TextInputType.url,
        onChanged: (v) => c.setField(f.key, v),
      ),
      FieldKind.text => TextBox(
        controller: box,
        enabled: !busy,
        onChanged: (v) => c.setField(f.key, v),
      ),
    };
    final caption = theme.typography.caption;
    return InfoLabel(
      label: f.required ? f.label : '${f.label} (optional)',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          input,
          if (f.hint.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(f.hint, style: caption?.copyWith(color: theme.secondaryText)),
          ],
          if (problem != null) ...[
            const SizedBox(height: 4),
            Text(problem, style: caption?.copyWith(color: theme.resources.systemFillColorCritical)),
          ],
        ],
      ),
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
              "they'll show up in ${c.chosen.rootLabel} → ${c.driveFolder.trim()}.",
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
          child: Button(
            onPressed: c.openFolder,
            child: Text(c.adding ? 'Open Folder' : 'Open CloudDrop Folder'),
          ),
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
