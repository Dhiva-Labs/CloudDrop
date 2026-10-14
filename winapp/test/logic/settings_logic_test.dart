import 'package:clouddrop_app/logic/settings_logic.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('auto-free labels', () {
    expect(autoFreeLabel(null), 'Never');
    expect(autoFreeLabel(0), 'Right after uploading');
    expect(autoFreeLabel(1), 'After 1 day');
    expect(autoFreeLabel(7), 'After 7 days');
    expect(autoFreeLabel(90), 'After 90 days');
  });

  test('presets are offered as they are', () {
    for (final preset in autoFreePresets) {
      expect(autoFreeOptions(preset), autoFreePresets);
    }
    expect(autoFreePresets, [null, 0, 7, 30, 90]);
  });

  test('custom values keep their place in the list', () {
    expect(autoFreeOptions(14), [null, 0, 7, 14, 30, 90]);
    expect(autoFreeOptions(365), [null, 0, 7, 30, 90, 365]);
    expect(autoFreeOptions(3), [null, 0, 3, 7, 30, 90]);
    expect(autoFreeLabel(14), 'After 14 days');
  });

  test('config set value for each choice (never / now / N)', () {
    expect(autoFreeCliValue(null), 'never');
    expect(autoFreeCliValue(0), 'now');
    expect(autoFreeCliValue(7), '7');
    expect(autoFreeCliValue(90), '90');
  });

  test('keep summary counts folders', () {
    expect(keepSummary(0), 'None');
    expect(keepSummary(1), '1 folder');
    expect(keepSummary(3), '3 folders');
  });

  test('comma lists round trip', () {
    expect(parseCommaList(' *.tmp, *.bak ,, '), ['*.tmp', '*.bak']);
    expect(commaList(['*.tmp', '*.bak']), '*.tmp, *.bak');
    expect(parseCommaList(''), isEmpty);
  });
}
