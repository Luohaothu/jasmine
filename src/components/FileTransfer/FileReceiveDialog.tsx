import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { useTransferStore } from '../../stores/transferStore';
import styles from './FileReceiveDialog.module.css';

function formatBytes(bytes: number) {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

export function FileReceiveDialog() {
  const { t } = useTranslation();
  const activeOffers = useTransferStore((state) => state.activeOffers);
  const removeOffer = useTransferStore((state) => state.removeOffer);

  if (!activeOffers || activeOffers.length === 0) {
    return null;
  }

  const offer = activeOffers[0];

  const handleAccept = async () => {
    try {
      await invoke('accept_file', { offer_id: offer.id });
    } catch (e) {
      console.error('Failed to accept file:', e);
    } finally {
      removeOffer(offer.id);
    }
  };

  const handleReject = async () => {
    try {
      await invoke('reject_file', { offer_id: offer.id });
    } catch (e) {
      console.error('Failed to reject file:', e);
    } finally {
      removeOffer(offer.id);
    }
  };

  return (
    <div className={styles.dialogOverlay}>
      <div className={styles.dialog}>
        <h3 className={styles.title}>{t('transfer.fileReceiveDialog.title')}</h3>
        <p className={styles.info}>
          <span className={styles.filename}>{offer.filename}</span>
          <br />
          {formatBytes(offer.size)}
          <br />
          {t('transfer.fileReceiveDialog.from', { sender: offer.senderId })}
        </p>
        <div className={styles.actions}>
          <button
            className={`${styles.btn} ${styles.btnReject}`}
            onClick={handleReject}
            type="button"
          >
            {t('transfer.actions.reject')}
          </button>
          <button
            className={`${styles.btn} ${styles.btnAccept}`}
            onClick={handleAccept}
            type="button"
          >
            {t('transfer.actions.accept')}
          </button>
        </div>
      </div>
    </div>
  );
}
