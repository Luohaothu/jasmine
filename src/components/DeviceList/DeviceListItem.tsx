import { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import styles from './DeviceListItem.module.css';
import { Peer } from '../../types/peer';

interface Props {
  peer: Peer;
}

interface PeerFingerprintInfo {
  peerId: string;
  fingerprint: string;
  verified: boolean;
  firstSeen: number;
}

function formatFingerprint(fingerprint: string): string {
  const normalized = fingerprint.replace(/\s+/g, '').toUpperCase();
  return normalized.match(/.{1,4}/g)?.join(' ') ?? fingerprint;
}

export const DeviceListItem = ({ peer }: Props) => {
  const { t } = useTranslation();
  const initial = peer.name ? peer.name.charAt(0).toUpperCase() : '?';
  const [fingerprintInfo, setFingerprintInfo] = useState<PeerFingerprintInfo | null>(null);

  useEffect(() => {
    invoke<PeerFingerprintInfo>('get_peer_fingerprint', { peerId: peer.id })
      .then(setFingerprintInfo)
      .catch((error) => {
        void error;
        setFingerprintInfo(null);
      });
  }, [peer.id]);

  const formattedFingerprint = useMemo(
    () => (fingerprintInfo ? formatFingerprint(fingerprintInfo.fingerprint) : null),
    [fingerprintInfo]
  );

  const toggleVerified = async () => {
    if (!fingerprintInfo) {
      return;
    }

    const verified = !fingerprintInfo.verified;
    try {
      await invoke('toggle_peer_verified', { peerId: peer.id, verified });
      setFingerprintInfo({ ...fingerprintInfo, verified });
    } catch (error) {
      void error;
    }
  };

  return (
    <div className={styles.item}>
      <Link to={`/chat/${peer.id}`} className={styles.linkArea} aria-label={peer.name}>
        <div className={styles.avatar}>{initial}</div>
        <div className={styles.info}>
          <span className={styles.name}>{peer.name}</span>
          <div className={styles.status}>
            <span className={`${styles.dot} ${styles[peer.status]}`} />
            <span className={styles.statusText}>{t(`devices.status.${peer.status}`)}</span>
          </div>
          {peer.warning && <div className={styles.warningText}>{peer.warning}</div>}
          {formattedFingerprint && (
            <div className={styles.securityMeta}>
              <span className={styles.fingerprint} data-testid="peer-fingerprint">
                {formattedFingerprint}
              </span>
              <span
                className={`${styles.verifiedBadge} ${fingerprintInfo?.verified ? styles.verified : styles.unverified}`}
              >
                {fingerprintInfo?.verified
                  ? t('devices.verification.verified')
                  : t('devices.verification.unverified')}
              </span>
            </div>
          )}
        </div>
      </Link>
      {fingerprintInfo && (
        <button
          type="button"
          className={styles.verifyToggle}
          data-testid="verify-toggle"
          onClick={toggleVerified}
          aria-label={
            fingerprintInfo.verified
              ? t('devices.verification.unverifyPeer', { name: peer.name })
              : t('devices.verification.verifyPeer', { name: peer.name })
          }
        >
          {fingerprintInfo.verified
            ? t('devices.verification.unverify')
            : t('devices.verification.verify')}
        </button>
      )}
    </div>
  );
};
