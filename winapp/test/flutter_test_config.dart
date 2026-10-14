import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';

/// Applies to every test in this directory.
///
/// The goldens are generated on a developer machine and compared on CI (Ubuntu runners with the
/// same Flutter version and the fonts in test/fonts). Everything that could differ is pinned in
/// the tests themselves (clock, time zone, window size, fonts, locale); this comparator only
/// forgives a handful of stray anti-aliasing pixels, so a real change (a moved or reworded label,
/// another colour) still fails.
Future<void> testExecutable(FutureOr<void> Function() testMain) async {
  setUpAll(() {
    final current = goldenFileComparator;
    if (current is LocalFileComparator) {
      goldenFileComparator = _TolerantComparator(current.basedir);
    }
  });
  await testMain();
}

/// Fraction of pixels that may differ.
const double _tolerance = 0.002;

class _TolerantComparator extends LocalFileComparator {
  _TolerantComparator(Uri basedir) : super(basedir.resolve('golden_test.dart'));

  @override
  Future<bool> compare(Uint8List imageBytes, Uri golden) async {
    final result = await GoldenFileComparator.compareLists(
      imageBytes,
      await getGoldenBytes(golden),
    );
    if (result.passed) return true;
    if (result.diffPercent <= _tolerance) {
      debugPrint(
        'golden $golden differs by ${(result.diffPercent * 100).toStringAsFixed(3)}% '
        '(within the allowed ${(_tolerance * 100).toStringAsFixed(1)}%)',
      );
      return true;
    }
    final error = await generateFailureOutput(result, golden, basedir);
    throw FlutterError(error);
  }
}
