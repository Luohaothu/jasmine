import React from 'react';
import { useTranslation } from 'react-i18next';
import { MonitorIcon } from './icons';
import styles from './Call.module.css';

export interface ScreenShareIndicatorProps {
  onStopSharing: () => void;
}

export const ScreenShareIndicator: React.FC<ScreenShareIndicatorProps> = ({ onStopSharing }) => {
  const { t } = useTranslation();

  return (
    <div className={styles.screenShareBanner} data-testid="screen-share-indicator">
      <div className={styles.screenShareBannerContent}>
        <MonitorIcon className={styles.iconSmall} />
        <span>{t('call.status.screenSharing')}</span>
        <button
          type="button"
          className={styles.stopShareBtn}
          onClick={onStopSharing}
          data-testid="btn-stop-share"
        >
          {t('call.action.stopScreenShare')}
        </button>
      </div>
    </div>
  );
};
