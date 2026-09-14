import assert from 'node:assert/strict';
import test from 'node:test';
import { dashboardErrorDetail } from './errors.ts';
import { setLocale } from '../i18n/index.ts';

test('known recoverable errors use the active locale while unknown diagnostics survive', () => {
  setLocale('zh-CN');
  assert.equal(dashboardErrorDetail(new Error('migration password is incorrect or the backup file is damaged')), '迁移包密码错误或文件损坏，请检查密码或重新选择备份文件。');
  assert.equal(dashboardErrorDetail('CPA Management Key is required'), '请填写 CPA Management Key，然后重新测试连接。');
  assert.equal(dashboardErrorDetail(new Error('upstream detail: example')), 'upstream detail: example');
  setLocale('en-US');
  assert.equal(dashboardErrorDetail('CPA managed runtime is not installed'), 'CPA is not installed. Install it from Overview first.');
  setLocale('zh-CN');
});
