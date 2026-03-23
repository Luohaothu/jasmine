import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTransferStore } from '../../stores/transferStore';
import { Transfer } from '../../stores/transferStore';
import styles from './FileTransferPanel.module.css';

interface TransferItemProps {
  transfer: Transfer;
}

function formatBytes(bytes: number) {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function formatSpeed(speed: number) {
  return formatBytes(speed) + '/s';
}

export function TransferItem({ transfer }: TransferItemProps) {
  const resumeTransfer = useTransferStore((state) => state.resumeTransfer);
  const retryTransfer = useTransferStore((state) => state.retryTransfer);
  const [isActionPending, setIsActionPending] = useState(false);

  const handleCancel = async () => {
    try {
      await invoke('cancel_transfer', { transfer_id: transfer.id });
    } catch (e) {
      void e;
    }
  };

  const handleResume = async () => {
    try {
      setIsActionPending(true);
      await resumeTransfer(transfer.id);
    } catch {
      setIsActionPending(false);
    }
  };

  const handleRetry = async () => {
    try {
      setIsActionPending(true);
      await retryTransfer(transfer.id);
    } catch {
      setIsActionPending(false);
    }
  };

  const isTerminal = ['completed', 'failed', 'partially-failed', 'cancelled', 'rejected'].includes(
    transfer.state
  );
  const isActive = transfer.state === 'active';
  const isFailed = transfer.state === 'failed';
  const isCancelled = transfer.state === 'cancelled';

  const showResume =
    isFailed &&
    transfer.resumable === true &&
    (transfer.bytes_transferred !== undefined ? transfer.bytes_transferred > 0 : true);
  const showRetry = isFailed || isCancelled;

  return (
    <div className={styles.item} data-testid={`transfer-item-${transfer.id}`}>
      <div className={styles.itemHeader}>
        <span className={styles.filename} title={transfer.filename}>
          {transfer.filename}
        </span>
        <span className={`${styles.status} ${styles[`status-${transfer.state}`]}`}>
          {transfer.state}
        </span>
      </div>

      <div className={styles.progressContainer}>
        <div
          className={`${styles.progressBar} ${styles[`progress-${transfer.state}`]}`}
          style={{ width: `${transfer.progress * 100}%` }}
          data-testid={`progress-${transfer.id}`}
        />
      </div>

      <div className={styles.itemFooter}>
        <span>{formatBytes(transfer.size)}</span>
        {isActive && <span className={styles.speed}>{formatSpeed(transfer.speed)}</span>}
        <div className={styles.itemActions}>
          {!isTerminal && (
            <button
              className={styles.cancelBtn}
              onClick={handleCancel}
              aria-label="Cancel transfer"
              type="button"
              disabled={isActionPending}
            >
              Cancel
            </button>
          )}
          {showResume && (
            <button
              className={styles.actionBtn}
              onClick={handleResume}
              aria-label="Resume transfer"
              type="button"
              disabled={isActionPending}
              data-testid="resume-btn"
            >
              Resume
            </button>
          )}
          {showRetry && (
            <button
              className={styles.secondaryBtn}
              onClick={handleRetry}
              aria-label="Retry transfer"
              type="button"
              disabled={isActionPending}
              data-testid="retry-btn"
            >
              Retry
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
