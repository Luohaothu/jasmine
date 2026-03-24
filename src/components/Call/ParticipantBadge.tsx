import React from 'react';
import { useTranslation } from 'react-i18next';
import { MicOffIcon } from './icons';
import styles from './Call.module.css';

interface ParticipantBadgeProps {
  name: string;
  isMuted?: boolean;
  status?: 'connecting' | 'connected' | 'disconnected' | 'new' | 'closed';
}

export const ParticipantBadge: React.FC<ParticipantBadgeProps> = ({
  name,
  isMuted,
  status = 'connected',
}) => {
  const { t } = useTranslation();

  return (
    <div className={styles.participantBadge} data-testid={`participant-badge-${name}`}>
      <div
        className={`${styles.participantStatus} ${styles[status] || ''}`}
        title={t(`call.participantBadge.status.${status}`)}
        data-testid="participant-status-indicator"
      />
      <span>{name}</span>
      {isMuted && (
        <div className={styles.mutedIcon} data-testid="participant-muted-icon">
          <MicOffIcon className={styles.icon} />
        </div>
      )}
    </div>
  );
};
