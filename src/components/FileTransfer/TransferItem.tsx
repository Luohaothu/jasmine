import { invoke } from "@tauri-apps/api/core";
import { Transfer } from "../../stores/transferStore";
import styles from "./FileTransferPanel.module.css";

interface TransferItemProps {
  transfer: Transfer;
}

function formatBytes(bytes: number) {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
}

function formatSpeed(speed: number) {
  return formatBytes(speed) + "/s";
}

export function TransferItem({ transfer }: TransferItemProps) {
  const handleCancel = async () => {
    try {
      await invoke("cancel_transfer", { transfer_id: transfer.id });
    } catch (e) {
      void e;
    }
  };

  const isTerminal = ["completed", "failed", "partially-failed", "cancelled", "rejected"].includes(transfer.state);
  const isActive = transfer.state === "active";

  return (
    <div className={styles.item} data-testid={`transfer-item-${transfer.id}`}>
      <div className={styles.itemHeader}>
        <span className={styles.filename} title={transfer.filename}>{transfer.filename}</span>
        <span className={`${styles.status} ${styles[`status-${transfer.state}`]}`}>{transfer.state}</span>
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
        {!isTerminal && (
          <button className={styles.cancelBtn} onClick={handleCancel} aria-label="Cancel transfer">
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}
