import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';

const getSavedLanguage = () => {
  try {
    if (
      typeof window !== 'undefined' &&
      window.localStorage &&
      typeof window.localStorage.getItem === 'function'
    ) {
      return window.localStorage.getItem('jasmine-language');
    }
  } catch {
    return null;
  }
  return null;
};

const savedLanguage = getSavedLanguage();
const browserLanguage =
  typeof navigator !== 'undefined' && navigator.language ? navigator.language : 'en';
const defaultLanguage = savedLanguage || (browserLanguage.startsWith('zh') ? 'zh-CN' : 'en');

i18n.use(initReactI18next).init({
  resources: {
    en: {
      translation: en,
    },
    'zh-CN': {
      translation: zhCN,
    },
  },
  lng: defaultLanguage,
  fallbackLng: 'en',
  interpolation: {
    escapeValue: false,
  },
});

export default i18n;
