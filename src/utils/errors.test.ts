import assert from 'node:assert/strict';
import test from 'node:test';
import { dashboardErrorDetail } from './errors.ts';
import { setLocale } from '../i18n/index.ts';

test('known recoverable errors use the active locale while unknown diagnostics survive', () => {
  const migrationError = 'migration password is incorrect or the backup file is damaged';
  const cpaKeyError = 'CPA Management Key is required';
  const cpaRuntimeError = 'CPA managed runtime is not installed';
  const unknownDiagnostic = 'upstream detail: example';

  setLocale('zh-CN');
  const migrationZh = dashboardErrorDetail(new Error(migrationError));
  assert.notEqual(migrationZh, migrationError, 'known errors must be localized, not raw backend English');
  assert.notEqual(dashboardErrorDetail(cpaKeyError), cpaKeyError);
  // Unknown diagnostics pass through verbatim so backend detail survives.
  assert.equal(dashboardErrorDetail(new Error(unknownDiagnostic)), unknownDiagnostic);

  setLocale('en-US');
  const migrationEn = dashboardErrorDetail(migrationError);
  assert.notEqual(migrationEn, migrationError);
  assert.notEqual(migrationEn, migrationZh, 'localized detail must follow the active locale');
  assert.notEqual(dashboardErrorDetail(cpaRuntimeError), cpaRuntimeError);
  setLocale('zh-CN');
});
