import 'dart:io';

import 'package:clouddrop_app/logic/about.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('the About version is the pubspec version', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final version = RegExp(r'^version:\s*([0-9.]+)', multiLine: true).firstMatch(pubspec)!.group(1);
    expect(appVersion, version);
  });
}
