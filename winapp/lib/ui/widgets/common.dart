/// Small building blocks shared by the screens.
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../data/models.dart';
import '../../logic/activity_text.dart';
import '../../logic/fmt.dart';
import '../scope.dart';
import '../theme.dart';

/// A Windows 11 card: rounded, thin border, card fill.
class Surface extends StatelessWidget {
  const Surface({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(16),
    this.radius = 8,
    this.color,
  });

  final Widget child;
  final EdgeInsetsGeometry padding;
  final double radius;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: color ?? theme.cardFill,
        borderRadius: BorderRadius.circular(radius),
        border: Border.all(color: theme.cardStroke),
      ),
      child: Padding(padding: padding, child: child),
    );
  }
}

/// A round, tinted badge around an icon.
class IconBadge extends StatelessWidget {
  const IconBadge({
    super.key,
    required this.icon,
    required this.tint,
    this.size = 36,
    this.iconSize,
  });

  final IconData icon;
  final Tint tint;
  final double size;
  final double? iconSize;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(color: tint.bg, shape: BoxShape.circle),
      alignment: Alignment.center,
      child: Icon(icon, size: iconSize ?? size * 0.46, color: tint.fg),
    );
  }
}

Tint tintFor(FluentThemeData theme, Tone tone) => switch (tone) {
  Tone.ok => theme.success,
  Tone.info => theme.neutral,
  Tone.warn => theme.critical,
};

/// The status chip of a row ("Uploaded", "Failed", ...).
class StatusPill extends StatelessWidget {
  const StatusPill(this.status, {super.key, this.tooltip});

  final Status status;
  final String? tooltip;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final tint = switch (status) {
      Status.uploaded => theme.success,
      Status.cloudOnly => theme.neutral,
      Status.waiting => theme.caution,
      Status.uploading => theme.accent,
      Status.failed => theme.critical,
    };
    final icon = switch (status) {
      Status.uploaded => FluentIcons.completed_solid,
      Status.cloudOnly => FluentIcons.cloud,
      Status.waiting => FluentIcons.clock,
      Status.uploading => FluentIcons.cloud_upload,
      Status.failed => FluentIcons.error_badge,
    };
    final pill = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(color: tint.bg, borderRadius: BorderRadius.circular(999)),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 11, color: tint.fg),
          const SizedBox(width: 5),
          Flexible(
            child: Text(
              status.label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.typography.caption?.copyWith(
                color: tint.fg,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
        ],
      ),
    );
    if (tooltip == null || tooltip!.isEmpty) return pill;
    return Tooltip(message: tooltip!, child: pill);
  }
}

/// A file-type icon in Windows-ish colours; folders are the yellow folder.
class FileTypeIcon extends StatelessWidget {
  const FileTypeIcon({
    super.key,
    required this.name,
    required this.isDir,
    this.dim = false,
    this.size = 22,
  });

  final String name;
  final bool isDir;
  final bool dim;
  final double size;

  @override
  Widget build(BuildContext context) {
    final (icon, color) = isDir
        ? (FluentIcons.fabric_folder_fill, const Color(0xFFF2B63C))
        : _forName(name);
    return Opacity(
      opacity: dim ? 0.55 : 1,
      child: Icon(icon, size: size, color: color),
    );
  }

  static (IconData, Color) _forName(String name) {
    final i = name.lastIndexOf('.');
    final ext = i < 0 ? '' : name.substring(i + 1).toLowerCase();
    switch (ext) {
      case 'jpg' ||
          'jpeg' ||
          'png' ||
          'gif' ||
          'webp' ||
          'bmp' ||
          'heic' ||
          'svg' ||
          'tif' ||
          'tiff':
        return (FluentIcons.photo2, const Color(0xFF2E9E8F));
      case 'mp4' || 'mkv' || 'mov' || 'avi' || 'webm' || 'wmv':
        return (FluentIcons.video, const Color(0xFF8764B8));
      case 'mp3' || 'wav' || 'flac' || 'ogg' || 'm4a' || 'aac':
        return (FluentIcons.music_note, const Color(0xFFD13438));
      case 'pdf':
        return (FluentIcons.pdf, const Color(0xFFD13438));
      case 'zip' || 'rar' || '7z' || 'tar' || 'gz' || 'xz':
        return (FluentIcons.package, const Color(0xFFCA8A04));
      case 'doc' || 'docx' || 'odt' || 'rtf':
        return (FluentIcons.text_document, const Color(0xFF2B579A));
      case 'xls' || 'xlsx' || 'ods' || 'csv':
        return (FluentIcons.excel_document, const Color(0xFF217346));
      case 'ppt' || 'pptx' || 'odp':
        return (FluentIcons.power_point_document, const Color(0xFFD24726));
      case 'txt' || 'md' || 'log' || 'json' || 'xml':
        return (FluentIcons.text_document, const Color(0xFF3B82C4));
      default:
        return (FluentIcons.page, const Color(0xFF7A8794));
    }
  }
}

/// A provider's picture: a generic icon by the kind of storage (no brand logos).
IconData providerIcon(String id) => switch (id) {
  'nextcloud' || 'webdav' => FluentIcons.fabric_network_folder,
  's3' => FluentIcons.blob_storage,
  'sftp' => FluentIcons.command_prompt,
  'mega' => FluentIcons.lock,
  _ => FluentIcons.cloud,
};

IconData activityIcon(ActivityIcon i) => switch (i) {
  ActivityIcon.uploaded => FluentIcons.cloud_upload,
  ActivityIcon.updated => FluentIcons.refresh,
  ActivityIcon.moved => FluentIcons.forward,
  ActivityIcon.keptInCloud => FluentIcons.cloud,
  ActivityIcon.freed => FluentIcons.broom,
  ActivityIcon.restored => FluentIcons.cloud_download,
  ActivityIcon.failed => FluentIcons.warning,
};

/// A scrolling page body, centred and limited in width.
class PageBody extends StatelessWidget {
  const PageBody({super.key, required this.children, this.maxWidth = 860});

  final List<Widget> children;
  final double maxWidth;

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, c) {
        return SingleChildScrollView(
          padding: pagePadding(c.maxWidth),
          child: Align(
            alignment: Alignment.topCenter,
            child: ConstrainedBox(
              constraints: BoxConstraints(maxWidth: maxWidth),
              child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children),
            ),
          ),
        );
      },
    );
  }
}

/// A progress bar that fills the width it is given (Fluent's own is a fixed 130 px by default).
class WideProgressBar extends StatelessWidget {
  const WideProgressBar({super.key, required this.fraction, this.color});

  /// 0..1
  final double fraction;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: double.infinity,
      child: ProgressBar(
        value: (fraction.clamp(0.0, 1.0)) * 100,
        activeColor: color,
        strokeWidth: 4,
      ),
    );
  }
}

/// The heading above a group of cards, with an optional link at the end ("See All").
class SectionTitle extends StatelessWidget {
  const SectionTitle(
    this.text, {
    super.key,
    this.padding = const EdgeInsets.only(top: 20, bottom: 8),
    this.trailing,
  });

  final String text;
  final EdgeInsets padding;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final title = Text(text, style: FluentTheme.of(context).typography.bodyStrong);
    if (trailing == null) return Padding(padding: padding, child: title);
    // The link's own padding would push the title down; centre both on one line instead.
    return Padding(
      padding: padding.copyWith(top: padding.top - 6, bottom: padding.bottom - 6),
      child: Row(
        children: [
          Expanded(child: title),
          trailing!,
        ],
      ),
    );
  }
}

/// One row of the Activity list: icon, what happened, where and when. Opens the file in Files.
class ActivityRow extends StatelessWidget {
  const ActivityRow({super.key, required this.activity, required this.onOpen});

  final Activity activity;
  final void Function(Activity a) onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = FluentTheme.of(context);
    final d = describe(activity, cloud: AppScope.of(context).app.provider.name);
    final sub = subtitle(activity, civilFromMs(activity.atMs));
    return Tooltip(
      message: activity.path,
      child: HoverButton(
        onPressed: () => onOpen(activity),
        builder: (context, states) => Container(
          color: states.isHovered ? theme.resources.subtleFillColorSecondary : Colors.transparent,
          padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
          child: Row(
            children: [
              IconBadge(icon: activityIcon(d.icon), tint: tintFor(theme, d.tone), size: 34),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(d.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                    const SizedBox(height: 2),
                    Text(
                      sub,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: theme.typography.caption?.copyWith(color: theme.secondaryText),
                    ),
                  ],
                ),
              ),
              const SizedBox(width: 8),
              Icon(FluentIcons.chevron_right, size: 11, color: theme.secondaryText),
            ],
          ),
        ),
      ),
    );
  }
}
