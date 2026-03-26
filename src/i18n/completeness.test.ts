import { describe, it, expect } from 'vitest';
import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';

function extractKeys(obj: Record<string, unknown>, prefix = ''): string[] {
  return Object.entries(obj).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return value && typeof value === 'object' && !Array.isArray(value)
      ? extractKeys(value as Record<string, unknown>, path)
      : [path];
  });
}

function collectEmptyKeys(obj: Record<string, unknown>, prefix = ''): string[] {
  return Object.entries(obj).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    if (value && typeof value === 'object' && !Array.isArray(value))
      return collectEmptyKeys(value as Record<string, unknown>, path);
    return value === '' ? [path] : [];
  });
}

describe('i18n completeness', () => {
  it('en.json has all keys from zh-CN.json', () => {
    const missing = extractKeys(en).filter((key) => !extractKeys(zhCN).includes(key));
    expect(missing, `Missing keys in zh-CN.json: ${missing.join(', ')}`).toEqual([]);
  });

  it('zh-CN.json has all keys from en.json', () => {
    const extra = extractKeys(zhCN).filter((key) => !extractKeys(en).includes(key));
    expect(extra, `Extra keys in zh-CN.json: ${extra.join(', ')}`).toEqual([]);
  });

  it('no empty string values in either locale', () => {
    const empty = [
      ...collectEmptyKeys(en).map((key) => `en.${key}`),
      ...collectEmptyKeys(zhCN).map((key) => `zh-CN.${key}`),
    ];
    expect(empty, `Empty string values: ${empty.join(', ')}`).toEqual([]);
  });
});
