import { useState } from "react";
import { useTransferStore } from "../../stores/transferStore";
import { TransferItem } from "./TransferItem";
import styles from "./FileTransferPanel.module.css";

export function FileTransferPanel() {
  const transfers = useTransferStore((state) => state.transfers);
  const [isOpen, setIsOpen] = useState(false);

  if (transfers.length === 0) {
    return null;
  }

  const activeCount = transfers.filter((t) => t.state === "active" || t.state === "queued").length;

  return (
    <div className={styles.panelContainer}>
      {isOpen && (
        <div className={styles.panel} data-testid="transfer-panel">
          <div className={styles.header}>
            <h3 className={styles.title}>File Transfers</h3>
            <button className={styles.closeBtn} onClick={() => setIsOpen(false)} aria-label="Minimize panel">×</button>
          </div>
          <div className={styles.list}>
            {transfers.map((t) => (
              <TransferItem key={t.id} transfer={t} />
            ))}
          </div>
        </div>
      )}

      <button className={styles.fab} onClick={() => setIsOpen(!isOpen)} aria-label="Toggle transfer panel">
        📁
        {activeCount > 0 && <span className={styles.badge} data-testid="active-badge">{activeCount}</span>}
      </button>
    </div>
  );
}
