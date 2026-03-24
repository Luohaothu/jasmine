import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import i18n from '../../i18n/i18n';
import LanguageSection from './LanguageSection';

describe('LanguageSection', () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    await i18n.changeLanguage('en');

    // Mock localStorage
    const localStorageMock = {
      getItem: vi.fn(),
      setItem: vi.fn(),
      clear: vi.fn(),
    };
    Object.defineProperty(window, 'localStorage', { value: localStorageMock, writable: true });
  });

  it('renders language selector and updates language on change', () => {
    const changeLanguageSpy = vi.spyOn(i18n, 'changeLanguage');

    render(<LanguageSection />);

    expect(screen.getByText('Language')).toBeInTheDocument();

    const select = screen.getByTestId('language-select');
    expect(select).toBeInTheDocument();
    expect((select as HTMLSelectElement).value).toBe('en');

    fireEvent.change(select, { target: { value: 'zh-CN' } });

    expect(changeLanguageSpy).toHaveBeenCalledWith('zh-CN');
    expect(window.localStorage.setItem).toHaveBeenCalledWith('jasmine-language', 'zh-CN');
  });
});
