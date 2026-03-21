import { invoke } from "@tauri-apps/api/core";

import { FolderTransfer } from "../../stores/folderTransferStore";
import styles from "./FolderTransferPanel.module.css";
import { useState } from "react";

interface FolderTransferItemProps {
  transfer: FolderTransfer;
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

export function FolderTransferItem({ transfer }: FolderTransferItemProps) {
  const [expanded, setExpanded] = useState(false);

  const handleCancel = async () => {
    try {
      await invoke("cancel_folder_transfer", { folderId: transfer.id });
    } catch (e) {
      console.error("Failed to cancel folder transfer:", e);
    }
  };

  const isTerminal = ["completed", "failed", "partially-failed", "cancelled", "rejected"].includes(transfer.state);
  const isActive = transfer.state === "active";
  const progressPct = transfer.totalSize > 0
    ? (transfer.transferredBytes / transfer.totalSize) * 100
    : 0;
  const displayFiles = transfer.files || [];

  return (
    <div className={styles.item} data-testid={`folder-transfer-item-${transfer.id}`}>
      <div className={styles.itemHeader} onClick={() => setExpanded(!expanded)} style={{ cursor: "pointer" }}>
        <span className={styles.filename} title={transfer.folderName}>📁 {transfer.folderName}</span>
        <span className={`${styles.status} ${styles[`status-${transfer.state}`]}`}>{transfer.state}</span>
      </div>

      <div className={styles.progressContainer}>
        <div
          className={`${styles.progressBar} ${styles[`progress-${transfer.state}`]}`}
          style={{ width: `${Math.min(100, progressPct)}%` }}
          data-testid={`folder-progress-${transfer.id}`}
        />
      </div>

      <div className={styles.itemFooter}>
        <span>{formatBytes(transfer.transferredBytes)} / {formatBytes(transfer.totalSize)}</span>
        {isActive && <span className={styles.speed}>{formatSpeed(transfer.speed)}</span>}
        {!isTerminal && (
          <button className={styles.cancelBtn} onClick={(e) => { e.stopPropagation(); handleCancel(); }} aria-label="Cancel transfer">
            Cancel
          </button>
        )}
      </div>

      {expanded && (
        <div className={styles.details} style={{ fontSize: "12px", marginTop: "8px", color: "var(--text-secondary, #666)" }}>
          <div style={{ marginBottom: "4px" }}>Files: {transfer.transferredFiles} / {transfer.totalFiles}</div>
          {displayFiles.length > 0 ? (
            <ul style={{ listStyleType: "none", padding: 0, margin: 0, maxHeight: "150px", overflowY: "auto", borderTop: "1px solid var(--border-color, #eee)" }} data-testid="folder-file-list">
              {displayFiles.map((file) => (
                <li key={file.id} style={{ padding: "4px 0", borderBottom: "1px solid var(--border-color, #eee)", display: "flex", flexDirection: "column" }}>
                  <div style={{ display: "flex", justifyContent: "space-between" }}>
                    <span style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", maxWidth: "70%" }} title={file.relativePath}>
                      {file.relativePath}
                    </span>
                    <span className={`${styles.status} ${styles[`status-${file.state}`]}`}>{file.state}</span>
                  </div>
                  <div style={{ display: "flex", justifyContent: "space-between", marginTop: "2px", opacity: 0.8, fontSize: "11px" }}>
                    <span>{formatBytes(file.transferredBytes)} / {formatBytes(file.size)}</span>
                    {file.state === "active" && <span>{formatSpeed(file.speed)}</span>}
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            isActive && transfer.currentFile && (
              <div style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }} title={transfer.currentFile}>
                Current: {transfer.currentFile}
              </div>
            )
          )}
        </div>
      )}
    </div>
  );
}
