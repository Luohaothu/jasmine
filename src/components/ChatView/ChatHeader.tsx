import React from 'react';
import { useTranslation } from 'react-i18next';
import { useCallSupport } from '../../hooks/useCallSupport';
import styles from './ChatHeader.module.css';

export interface ChatHeaderProps {
  peerName: string;
  status: 'online' | 'offline';
  onBack?: () => void;
  onAudioCall?: () => void;
  onVideoCall?: () => void;
}

const PhoneIcon = ({ className }: { className?: string }) => (
  <svg
    aria-hidden="true"
    focusable="false"
    className={className}
    width="20"
    height="20"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z" />
  </svg>
);

const VideoIcon = ({ className }: { className?: string }) => (
  <svg
    aria-hidden="true"
    focusable="false"
    className={className}
    width="20"
    height="20"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <polygon points="23 7 16 12 23 17 23 7" />
    <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
  </svg>
);

export const ChatHeader: React.FC<ChatHeaderProps> = ({
  peerName,
  status,
  onBack,
  onAudioCall,
  onVideoCall,
}) => {
  const { t } = useTranslation();
  const { supported } = useCallSupport();

  return (
    <header className={styles.header}>
      {onBack && (
        <button
          type="button"
          className={styles.backButton}
          onClick={onBack}
          aria-label={t('devices.chatHeader.back')}
        >
          <svg
            aria-hidden="true"
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M19 12H5M12 19l-7-7 7-7" />
          </svg>
        </button>
      )}
      <div className={styles.info}>
        <div className={styles.name}>{peerName}</div>
        <div className={styles.status}>
          <span
            className={`${styles.statusIndicator} ${
              status === 'online' ? styles.online : styles.offline
            }`}
          />
          {t(`devices.status.${status}`)}
        </div>
      </div>

      {supported && (
        <div className={styles.actions}>
          {onAudioCall && (
            <button
              type="button"
              className={styles.actionButton}
              onClick={onAudioCall}
              aria-label={t('call.action.audioCall')}
              title={t('call.action.audioCall')}
            >
              <PhoneIcon />
            </button>
          )}
          {onVideoCall && (
            <button
              type="button"
              className={styles.actionButton}
              onClick={onVideoCall}
              aria-label={t('call.action.videoCall')}
              title={t('call.action.videoCall')}
            >
              <VideoIcon />
            </button>
          )}
        </div>
      )}
    </header>
  );
};
