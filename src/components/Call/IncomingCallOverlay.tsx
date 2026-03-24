import React from 'react';
import { useTranslation } from 'react-i18next';
import { PhoneIcon, PhoneOffIcon } from './icons';
import styles from './Call.module.css';

export interface IncomingCallOverlayProps {
  peerId: string;
  onAccept: () => void;
  onReject: () => void;
}

export const IncomingCallOverlay: React.FC<IncomingCallOverlayProps> = ({
  peerId,
  onAccept,
  onReject,
}) => {
  const { t } = useTranslation();

  return (
    <div className={styles.overlay} data-testid="incoming-call-overlay">
      <div className={styles.audioAvatar}>{peerId.charAt(0).toUpperCase()}</div>
      <div className={styles.title}>{peerId}</div>
      <div className={styles.status}>{t('call.status.incoming')}</div>

      <div className={styles.controls}>
        <button
          type="button"
          className={`${styles.btn} ${styles.btnAccept}`}
          onClick={onAccept}
          aria-label={t('call.action.accept')}
          data-testid="btn-accept"
        >
          <PhoneIcon className={styles.icon} />
        </button>
        <button
          type="button"
          className={`${styles.btn} ${styles.btnReject}`}
          onClick={onReject}
          aria-label={t('call.action.reject')}
          data-testid="btn-reject"
        >
          <PhoneOffIcon className={styles.icon} />
        </button>
      </div>
    </div>
  );
};
