import React from 'react';
import { useTranslation } from 'react-i18next';
import { PhoneOffIcon } from './icons';
import styles from './Call.module.css';

export interface OutgoingCallOverlayProps {
  peerId: string;
  status: 'new' | 'connecting';
  onEnd: () => void;
}

export const OutgoingCallOverlay: React.FC<OutgoingCallOverlayProps> = ({
  peerId,
  status,
  onEnd,
}) => {
  const { t } = useTranslation();

  return (
    <div className={styles.overlay} data-testid="outgoing-call-overlay">
      <div className={styles.audioAvatar}>{peerId.charAt(0).toUpperCase()}</div>
      <div className={styles.title}>{peerId}</div>
      <div className={styles.status}>
        {status === 'connecting' ? t('call.status.connecting') : t('call.status.calling')}
      </div>

      <div className={styles.controls}>
        <button
          type="button"
          className={`${styles.btn} ${styles.btnReject}`}
          onClick={onEnd}
          aria-label={t('call.action.end')}
          data-testid="btn-end"
        >
          <PhoneOffIcon className={styles.icon} />
        </button>
      </div>
    </div>
  );
};
