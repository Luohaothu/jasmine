import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import ProfileSection from './ProfileSection';
import AppSettingsSection from './AppSettingsSection';
import SecuritySection from './SecuritySection';
import LanguageSection from './LanguageSection';
import styles from './Settings.module.css';

export default function Settings() {
  const { t } = useTranslation();
  const [deviceId, setDeviceId] = useState<string>('');

  useEffect(() => {
    invoke<{ device_id: string } | null>('get_identity')
      .then((res) => {
        if (res) {
          setDeviceId(res.device_id);
        }
      })
      .catch((error) => {
        void error;
      });
  }, []);

  return (
    <div className={styles.container}>
      <h1 className={styles.title}>{t('settings.title')}</h1>

      <ProfileSection />
      <LanguageSection />
      <AppSettingsSection />
      <SecuritySection />

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>{t('settings.about.title')}</h2>
        <div className={styles.aboutText}>
          <strong>{t('settings.about.versionLabel')}:</strong> 0.1.0
        </div>
        <div className={styles.aboutText}>
          <strong>{t('settings.about.appDeviceIdLabel')}:</strong> {deviceId || t('common.loading')}
        </div>
      </section>
    </div>
  );
}
