import React from 'react';
import { useTranslation } from 'react-i18next';
import styles from './Settings.module.css';

export default function LanguageSection() {
  const { t, i18n } = useTranslation();

  const handleLanguageChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const newLang = e.target.value;
    i18n.changeLanguage(newLang);
    try {
      if (
        typeof window !== 'undefined' &&
        window.localStorage &&
        typeof window.localStorage.setItem === 'function'
      ) {
        window.localStorage.setItem('jasmine-language', newLang);
      }
    } catch {
      return;
    }
  };

  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>{t('settings.app.languageTitle')}</h2>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="language-select">
          {t('settings.app.languageLabel')}
        </label>
        <select
          id="language-select"
          className={styles.select}
          value={i18n.resolvedLanguage || i18n.language || 'en'}
          onChange={handleLanguageChange}
          data-testid="language-select"
        >
          <option value="en">{t('settings.app.languages.en')}</option>
          <option value="zh-CN">{t('settings.app.languages.zhCN')}</option>
        </select>
      </div>
    </section>
  );
}
