import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
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
      fingerprintInfo ? formatFingerprint(fingerprintInfo.fingerprint) : 'Loading fingerprint...',
    [fingerprintInfo]
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
      <h2 className={styles.sectionTitle}>Security</h2>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="own-fingerprint">
          Local Fingerprint
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
            aria-label="Copy fingerprint"
            disabled={!fingerprintInfo}
          >
            Copy
          </button>
        </div>
      </div>

      <p className={styles.securityHint}>
        Compare this fingerprint with trusted peers to verify your secure identity.
      </p>
    </section>
  );
}
