import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

import { useFolderTransferStore } from "../../stores/folderTransferStore";
import styles from "./FolderReceiveDialog.module.css";

interface AppSettingsPayload {
  download_dir: string;
}

function formatBytes(bytes: number) {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + " " + sizes[i];
}

export function FolderReceiveDialog() {
  const activeOffers = useFolderTransferStore((state) => state.activeFolderOffers);
  const addFolderTransfer = useFolderTransferStore((state) => state.addFolderTransfer);
  const removeOffer = useFolderTransferStore((state) => state.removeFolderOffer);
  const removeFolderTransfer = useFolderTransferStore((state) => state.removeFolderTransfer);

  if (!activeOffers || activeOffers.length === 0) {
    return null;
  }

  const offer = activeOffers[0];

  const handleAccept = async () => {
    try {
      const settings = await invoke<AppSettingsPayload>("get_settings");

      const targetDir = await open({
        directory: true,
        multiple: false,
        defaultPath: settings.download_dir,
        title: "Select Download Location",
      }) as string | null;

      if (!targetDir) {
        return;
      }

      addFolderTransfer({
        id: offer.id,
        folderName: offer.folderName,
        totalSize: offer.totalSize,
        totalFiles: offer.totalFiles,
        transferredBytes: 0,
        transferredFiles: 0,
        speed: 0,
        state: "queued",
        senderName: offer.senderName,
        files: [],
      });

      await invoke("accept_folder_transfer", {
        folderId: offer.id,
        targetDir,
      });
      removeOffer(offer.id);
    } catch (e) {
      removeFolderTransfer(offer.id);
      console.error("Failed to accept folder transfer:", e);
    }
  };

  const handleReject = async () => {
    try {
      await invoke("reject_folder_transfer", { folderId: offer.id });
      removeOffer(offer.id);
    } catch (e) {
      console.error("Failed to reject folder transfer:", e);
    }
  };

  return (
    <div className={styles.dialogOverlay} data-testid="folder-receive-dialog">
      <div className={styles.dialog}>
        <h3 className={styles.title}>Folder Offer Received</h3>
        <p className={styles.info}>
          <span className={styles.filename}>{offer.folderName}</span>
          <br />
          {formatBytes(offer.totalSize)} • {offer.totalFiles} files
          <br />
          From: {offer.senderName}
        </p>
        <div className={styles.actions}>
          <button className={`${styles.btn} ${styles.btnReject}`} onClick={handleReject}>Reject</button>
          <button className={`${styles.btn} ${styles.btnAccept}`} onClick={handleAccept}>Accept</button>
        </div>
      </div>
    </div>
  );
}
