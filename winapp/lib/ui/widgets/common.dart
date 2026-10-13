/// Small building blocks shared by the screens.
library;

import 'package:fluent_ui/fluent_ui.dart';

import '../../logic/activity_text.dart';
import '../../logic/fmt.dart';
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
      case 'txt' || 'md' || 'log' || 'csv' || 'json' || 'xml' || 'doc' || 'docx' || 'odt' || 'rtf':
        return (FluentIcons.text_document, const Color(0xFF3B82C4));
      default:
        return (FluentIcons.page, const Color(0xFF7A8794));
    }
  }
}

IconData activityIcon(ActivityIcon i) => switch (i) {
  ActivityIcon.uploaded => FluentIcons.cloud_upload,
  ActivityIcon.updated => FluentIcons.refresh,
  ActivityIcon.moved => FluentIcons.forward,
  ActivityIcon.keptInCloud => FluentIcons.cloud,
  ActivityIcon.freed => FluentIcons.hard_drive,
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

/// The heading above a group of cards.
class SectionTitle extends StatelessWidget {
  const SectionTitle(
    this.text, {
    super.key,
    this.padding = const EdgeInsets.only(top: 20, bottom: 8),
  });

  final String text;
  final EdgeInsets padding;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: padding,
      child: Text(text, style: FluentTheme.of(context).typography.bodyStrong),
    );
  }
}
