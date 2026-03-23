import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import ProfileSection from './ProfileSection';
import AppSettingsSection from './AppSettingsSection';
import SecuritySection from './SecuritySection';
import styles from './Settings.module.css';

export default function Settings() {
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
      <h1 className={styles.title}>Settings</h1>

      <ProfileSection />
      <AppSettingsSection />
      <SecuritySection />

      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>About</h2>
        <div className={styles.aboutText}>
          <strong>Version:</strong> 0.1.0
        </div>
        <div className={styles.aboutText}>
          <strong>App Device ID:</strong> {deviceId || 'Loading...'}
        </div>
      </section>
    </div>
  );
}
