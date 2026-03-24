import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';

import { FolderTransfer, useFolderTransferStore } from '../../stores/folderTransferStore';
import styles from './FolderTransferPanel.module.css';
import React, { useState } from 'react';

interface FolderTransferItemProps {
  transfer: FolderTransfer;
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

export function FolderTransferItem({ transfer }: FolderTransferItemProps) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const resumeFolderTransfer = useFolderTransferStore((state) => state.resumeFolderTransfer);
  const retryFolderTransfer = useFolderTransferStore((state) => state.retryFolderTransfer);
  const [isActionPending, setIsActionPending] = useState(false);

  const handleCancel = async () => {
    try {
      await invoke('cancel_folder_transfer', { folderId: transfer.id });
    } catch (e) {
      console.error('Failed to cancel folder transfer:', e);
    }
  };

  const handleResume = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      setIsActionPending(true);
      await resumeFolderTransfer(transfer.id);
    } catch (err) {
      console.error('Failed to resume folder transfer:', err);
      setIsActionPending(false);
    }
  };

  const handleRetry = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      setIsActionPending(true);
      await retryFolderTransfer(transfer.id);
    } catch (err) {
      console.error('Failed to retry folder transfer:', err);
      setIsActionPending(false);
    }
  };

  const isTerminal = ['completed', 'failed', 'partially-failed', 'cancelled', 'rejected'].includes(
    transfer.state
  );
  const isActive = transfer.state === 'active';
  const isFailed = transfer.state === 'failed';
  const isCancelled = transfer.state === 'cancelled';

  const showResume = isFailed && transfer.resumable === true && transfer.transferredBytes > 0;
  const showRetry = isFailed || isCancelled;

  const progressPct =
    transfer.totalSize > 0 ? (transfer.transferredBytes / transfer.totalSize) * 100 : 0;
  const displayFiles = transfer.files || [];

  return (
    <div className={styles.item} data-testid={`folder-transfer-item-${transfer.id}`}>
      <button
        type="button"
        className={styles.itemHeader}
        onClick={() => setExpanded(!expanded)}
        style={{
          cursor: 'pointer',
          background: 'none',
          border: 'none',
          width: '100%',
          textAlign: 'left',
          padding: 0,
        }}
      >
        <span className={styles.filename} title={transfer.folderName}>
          📁 {transfer.folderName}
        </span>
        <span className={`${styles.status} ${styles[`status-${transfer.state}`]}`}>
          {t(`transfer.status.${transfer.state}`)}
        </span>
      </button>

      <div className={styles.progressContainer}>
        <div
          className={`${styles.progressBar} ${styles[`progress-${transfer.state}`]}`}
          style={{ width: `${Math.min(100, progressPct)}%` }}
          data-testid={`folder-progress-${transfer.id}`}
        />
      </div>

      <div className={styles.itemFooter}>
        <span>
          {formatBytes(transfer.transferredBytes)} / {formatBytes(transfer.totalSize)}
        </span>
        {isActive && <span className={styles.speed}>{formatSpeed(transfer.speed)}</span>}
        <div className={styles.itemActions}>
          {!isTerminal && (
            <button
              className={styles.cancelBtn}
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                handleCancel();
              }}
              aria-label={t('transfer.aria.cancelTransfer')}
              disabled={isActionPending}
            >
              {t('transfer.actions.cancel')}
            </button>
          )}
          {showResume && (
            <button
              className={styles.actionBtn}
              type="button"
              onClick={handleResume}
              aria-label={t('transfer.aria.resumeTransfer')}
              disabled={isActionPending}
              data-testid="resume-btn"
            >
              {t('transfer.actions.resume')}
            </button>
          )}
          {showRetry && (
            <button
              className={styles.secondaryBtn}
              type="button"
              onClick={handleRetry}
              aria-label={t('transfer.aria.retryTransfer')}
              disabled={isActionPending}
              data-testid="retry-btn"
            >
              {t('transfer.actions.retry')}
            </button>
          )}
        </div>
      </div>

      {expanded && (
        <div
          className={styles.details}
          style={{ fontSize: '12px', marginTop: '8px', color: 'var(--text-secondary, #666)' }}
        >
          <div style={{ marginBottom: '4px' }}>
            {t('transfer.folderItem.filesSummary', {
              transferred: transfer.transferredFiles,
              total: transfer.totalFiles,
            })}
          </div>
          {displayFiles.length > 0 ? (
            <ul
              style={{
                listStyleType: 'none',
                padding: 0,
                margin: 0,
                maxHeight: '150px',
                overflowY: 'auto',
                borderTop: '1px solid var(--border-color, #eee)',
              }}
              data-testid="folder-file-list"
            >
              {displayFiles.map((file) => (
                <li
                  key={file.id}
                  style={{
                    padding: '4px 0',
                    borderBottom: '1px solid var(--border-color, #eee)',
                    display: 'flex',
                    flexDirection: 'column',
                  }}
                >
                  <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                    <span
                      style={{
                        whiteSpace: 'nowrap',
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        maxWidth: '70%',
                      }}
                      title={file.relativePath}
                    >
                      {file.relativePath}
                    </span>
                    <span className={`${styles.status} ${styles[`status-${file.state}`]}`}>
                      {t(`transfer.status.${file.state}`)}
                    </span>
                  </div>
                  <div
                    style={{
                      display: 'flex',
                      justifyContent: 'space-between',
                      marginTop: '2px',
                      opacity: 0.8,
                      fontSize: '11px',
                    }}
                  >
                    <span>
                      {formatBytes(file.transferredBytes)} / {formatBytes(file.size)}
                    </span>
                    {file.state === 'active' && <span>{formatSpeed(file.speed)}</span>}
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            isActive &&
            transfer.currentFile && (
              <div
                style={{ whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}
                title={transfer.currentFile}
              >
                {t('transfer.folderItem.currentFile', { file: transfer.currentFile })}
              </div>
            )
          )}
        </div>
      )}
    </div>
  );
}
