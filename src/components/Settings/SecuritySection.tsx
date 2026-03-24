import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import styles from './Settings.module.css';

interface OwnFingerprintInfo {
  deviceId: string;
  fingerprint: string;
}

function formatFingerprint(fingerprint: string): string {
  const normalized = fingerprint.replace(/\s+/g, '').toUpperCase();
  return normalized.match(/.{1,4}/g)?.join(' ') ?? fingerprint;
}

export default function SecuritySection() {
  const { t } = useTranslation();
  const [fingerprintInfo, setFingerprintInfo] = useState<OwnFingerprintInfo | null>(null);

  useEffect(() => {
    invoke<OwnFingerprintInfo>('get_own_fingerprint')
      .then(setFingerprintInfo)
      .catch((error) => {
        void error;
      });
  }, []);

  const formattedFingerprint = useMemo(
    () =>
      fingerprintInfo
        ? formatFingerprint(fingerprintInfo.fingerprint)
        : t('settings.security.loadingFingerprint'),
    [fingerprintInfo, t]
  );

  const copyFingerprint = () => {
    if (!fingerprintInfo) {
      return;
    }

    navigator.clipboard.writeText(formattedFingerprint).catch((error) => {
      void error;
    });
  };

  return (
    <section className={styles.section} data-testid="security-section">
      <h2 className={styles.sectionTitle}>{t('settings.security.title')}</h2>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="own-fingerprint">
          {t('settings.security.localFingerprintLabel')}
        </label>
        <div className={styles.row}>
          <div
            id="own-fingerprint"
            className={styles.fingerprintValue}
            data-testid="own-fingerprint"
          >
            {formattedFingerprint}
          </div>
          <button
            type="button"
            className={styles.iconButton}
            onClick={copyFingerprint}
            data-testid="copy-fingerprint"
            aria-label={t('settings.security.copyFingerprintAriaLabel')}
            disabled={!fingerprintInfo}
          >
            {t('settings.actions.copy')}
          </button>
        </div>
      </div>

      <p className={styles.securityHint}>{t('settings.security.hint')}</p>
    </section>
  );
}
