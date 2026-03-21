import { useState } from "react";
import { useFolderTransferStore } from "../../stores/folderTransferStore";
import { FolderTransferItem } from "./FolderTransferItem";
import styles from "./FolderTransferPanel.module.css";

export function FolderTransferPanel() {
  const transfers = useFolderTransferStore((state) => state.folderTransfers);
  const [isOpen, setIsOpen] = useState(false);

  if (transfers.length === 0) {
    return null;
  }

  const activeCount = transfers.filter((t) => t.state === "active" || t.state === "queued").length;

  return (
    <div className={styles.panelContainer} style={{ bottom: "88px" }}>
      {isOpen && (
        <div className={styles.panel} data-testid="folder-transfer-panel">
          <div className={styles.header}>
            <h3 className={styles.title}>Folder Transfers</h3>
            <button className={styles.closeBtn} onClick={() => setIsOpen(false)} aria-label="Minimize panel">×</button>
          </div>
          <div className={styles.list}>
            {transfers.map((t) => (
              <FolderTransferItem key={t.id} transfer={t} />
            ))}
          </div>
        </div>
      )}

      <button className={styles.fab} onClick={() => setIsOpen(!isOpen)} aria-label="Toggle folder transfer panel">
        📁
        {activeCount > 0 && <span className={styles.badge} data-testid="folder-active-badge">{activeCount}</span>}
      </button>
    </div>
  );
}
