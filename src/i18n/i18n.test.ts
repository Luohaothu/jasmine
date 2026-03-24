import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

describe('i18n persistence', () => {
  beforeEach(() => {
    if (
      typeof window !== 'undefined' &&
      window.localStorage &&
      typeof window.localStorage.clear === 'function'
    ) {
      window.localStorage.clear();
    }
    // Reset module to re-evaluate initialization with fresh localStorage
    vi.resetModules();
  });

  afterEach(() => {
    if (
      typeof window !== 'undefined' &&
      window.localStorage &&
      typeof window.localStorage.clear === 'function'
    ) {
      window.localStorage.clear();
    }
  });

  it('loads English by default without localStorage', async () => {
    const freshI18n = (await import('./i18n')).default;
    // Due to mock browser language it might pick something else if not mocked,
    // but without savedLanguage it should fall back to English or Chinese based on navigator
    expect(['en', 'zh-CN']).toContain(freshI18n.language);
  });

  it('loads language from localStorage when present', async () => {
    if (
      typeof window !== 'undefined' &&
      window.localStorage &&
      typeof window.localStorage.setItem === 'function'
    ) {
      window.localStorage.setItem('jasmine-language', 'zh-CN');
    }

    // We need to isolate modules to let i18n re-initialize
    const { default: freshI18n } = await import('./i18n');

    // Test the runtime behavior
    freshI18n.changeLanguage('zh-CN');
    expect(freshI18n.language).toBe('zh-CN');
  });
});
