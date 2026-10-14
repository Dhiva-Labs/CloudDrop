library;

import 'package:fluent_ui/fluent_ui.dart';

import '../theme.dart';

/// The title row of a screen ("Home", "Files", ...), aligned with the page contents.
class AppPageHeader extends StatelessWidget {
  const AppPageHeader(this.title, {super.key, this.trailing});

  final String title;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final w = MediaQuery.widthOf(context);
    final gutter = pageGutter(w);
    return Padding(
      padding: EdgeInsetsDirectional.only(start: gutter, end: gutter, bottom: 10),
      child: Row(
        children: [
          Expanded(child: Text(title, style: FluentTheme.of(context).typography.title)),
          ?trailing,
        ],
      ),
    );
  }
}
