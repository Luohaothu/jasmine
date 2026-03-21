import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

import { TransferState } from "./transferStore";

export interface FolderTransferFile {
  id: string;
  filename: string;
  relativePath: string;
  size: number;
  transferredBytes: number;
  progress: number;
  speed: number;
  state: TransferState;
}

export interface FolderTransfer {
  id: string;
  folderName: string;
  totalSize: number;
  totalFiles: number;
  transferredBytes: number;
  transferredFiles: number;
  speed: number;
  state: TransferState;
  senderName?: string;
  currentFile?: string;
  files: FolderTransferFile[];
}

export interface FolderOffer {
  id: string;
  folderName: string;
  totalSize: number;
  totalFiles: number;
  senderName: string;
}

interface FolderOfferEventPayload {
  folderTransferId: string;
  folderName: string;
  fileCount: number;
  totalSize: number;
  senderName: string;
}

interface FolderProgressEventPayload {
  folderTransferId: string;
  sentBytes: number;
  totalBytes: number;
  completedFiles: number;
  totalFiles: number;
}

interface FolderCompletedEventPayload {
  folderTransferId: string;
  status: string;
  failedFiles?: number;
}

interface TransferPayload {
  id: string;
  filename: string;
  size: number;
  progress: number;
  speed: number;
  state: string;
  folderId?: string;
  folderRelativePath?: string;
}

function upsertFolderTransfer(
  folderTransfers: FolderTransfer[],
  nextTransfer: FolderTransfer
): FolderTransfer[] {
  if (folderTransfers.some((transfer) => transfer.id === nextTransfer.id)) {
    return folderTransfers.map((transfer) =>
      transfer.id === nextTransfer.id ? { ...transfer, ...nextTransfer } : transfer
    );
  }

  return [...folderTransfers, nextTransfer];
}

function folderStatusToTransferState(status: string): TransferState {
  switch (status) {
    case "pending":
      return "queued";
    case "sending":
      return "active";
    case "completed":
      return "completed";
    case "cancelled":
      return "cancelled";
    case "rejected":
      return "rejected";
    case "partially-failed":
      return "partially-failed";
    case "failed":
    default:
      return "failed";
  }
}

function transferStatusToTransferState(status: string): TransferState {
  switch (status) {
    case "queued":
    case "active":
    case "completed":
    case "failed":
    case "cancelled":
    case "rejected":
    case "partially-failed":
      return status as TransferState;
    default:
      return "failed";
  }
}

function toTransferredBytes(size: number, progress: number): number {
  const normalizedProgress = Math.min(1, Math.max(0, progress));
  return Math.round(size * normalizedProgress);
}

function buildFolderFilesByTransferId(
  transfers: TransferPayload[]
): Map<string, FolderTransferFile[]> {
  const filesByTransferId = new Map<string, FolderTransferFile[]>();

  for (const transfer of transfers) {
    if (!transfer.folderId) {
      continue;
    }

    const relativePath = transfer.folderRelativePath || transfer.filename;
    const nextFile: FolderTransferFile = {
      id: transfer.id,
      filename: transfer.filename,
      relativePath,
      size: transfer.size,
      transferredBytes: toTransferredBytes(transfer.size, transfer.progress),
      progress: transfer.progress,
      speed: transfer.speed,
      state: transferStatusToTransferState(transfer.state),
    };

    const existingFiles = filesByTransferId.get(transfer.folderId) || [];
    existingFiles.push(nextFile);
    filesByTransferId.set(transfer.folderId, existingFiles);
  }

  for (const files of filesByTransferId.values()) {
    files.sort((left, right) => left.relativePath.localeCompare(right.relativePath));
  }

  return filesByTransferId;
}

function nextCurrentFile(files: FolderTransferFile[]): string | undefined {
  return (
    files.find((file) => file.state === "active")?.relativePath ||
    files.find((file) => file.state === "queued")?.relativePath
  );
}

async function refreshFolderTransferFiles() {
  try {
    const transfers = (await invoke<TransferPayload[]>("get_transfers")) || [];
    useFolderTransferStore.getState().syncFolderTransferFiles(transfers);
  } catch (error) {
    console.warn("Failed to sync folder transfer files:", error);
  }
}

/* eslint-disable no-unused-vars */
interface FolderTransferStore {
  folderTransfers: FolderTransfer[];
  activeFolderOffers: FolderOffer[];
  addFolderTransfer(transfer: FolderTransfer): void;
  removeFolderTransfer(id: string): void;
  updateFolderProgress(
    id: string,
    totalSize: number,
    totalFiles: number,
    transferredBytes: number,
    transferredFiles: number,
    speed: number,
    currentFile?: string
  ): void;
  updateFolderState(id: string, state: TransferState): void;
  cancelFolderTransfer(id: string): void;
  addFolderOffer(offer: FolderOffer): void;
  removeFolderOffer(id: string): void;
  syncFolderTransferFiles(transfers: TransferPayload[]): void;
}
/* eslint-enable no-unused-vars */

export const useFolderTransferStore = create<FolderTransferStore>((set) => ({
  folderTransfers: [],
  activeFolderOffers: [],
  addFolderTransfer: (transfer) => set((state) => ({
    folderTransfers: upsertFolderTransfer(state.folderTransfers, transfer),
  })),
  removeFolderTransfer: (id) => set((state) => ({
    folderTransfers: state.folderTransfers.filter((transfer) => transfer.id !== id),
  })),
  updateFolderProgress: (id, totalSize, totalFiles, transferredBytes, transferredFiles, speed, currentFile) =>
    set((state) => {
      const existingTransfer = state.folderTransfers.find((transfer) => transfer.id === id);
      return {
        folderTransfers: upsertFolderTransfer(state.folderTransfers, {
          id,
          folderName: existingTransfer?.folderName || id,
          totalSize,
          totalFiles,
          transferredBytes,
          transferredFiles,
          speed,
          state: "active",
          currentFile,
          senderName: existingTransfer?.senderName,
          files: existingTransfer?.files || [],
        }),
      };
    }),
  updateFolderState: (id, transferState) => set((state) => ({
    folderTransfers: state.folderTransfers.map((transfer) =>
      transfer.id === id ? { ...transfer, state: transferState } : transfer
    ),
  })),
  cancelFolderTransfer: (id) => set((state) => ({
    folderTransfers: state.folderTransfers.map((transfer) =>
      transfer.id === id ? { ...transfer, state: "cancelled" } : transfer
    ),
  })),
  addFolderOffer: (offer) => set((state) => ({
    activeFolderOffers: [...state.activeFolderOffers, offer],
  })),
  removeFolderOffer: (id) => set((state) => ({
    activeFolderOffers: state.activeFolderOffers.filter((offer) => offer.id !== id),
  })),
  syncFolderTransferFiles: (transfers) => set((state) => {
    const filesByTransferId = buildFolderFilesByTransferId(transfers);

    return {
      folderTransfers: state.folderTransfers.map((transfer) => {
        const files = filesByTransferId.get(transfer.id) || transfer.files;
        const currentFile = nextCurrentFile(files);

        return {
          ...transfer,
          currentFile: currentFile ?? (files.length === 0 ? transfer.currentFile : undefined),
          files,
        };
      }),
    };
  }),
}));

export async function setupFolderTransferListeners(): Promise<() => void> {
  const refreshFiles = () => {
    void refreshFolderTransferFiles();
  };

  await refreshFolderTransferFiles();

  const unlistenOffer = await listen<FolderOfferEventPayload>("folder-offer-received", (event) => {
    useFolderTransferStore.getState().addFolderOffer({
      id: event.payload.folderTransferId,
      folderName: event.payload.folderName,
      totalSize: event.payload.totalSize,
      totalFiles: event.payload.fileCount,
      senderName: event.payload.senderName,
    });
  });

  const unlistenProgress = await listen<FolderProgressEventPayload>("folder-progress", (event) => {
    useFolderTransferStore.getState().updateFolderProgress(
      event.payload.folderTransferId,
      event.payload.totalBytes,
      event.payload.totalFiles,
      event.payload.sentBytes,
      event.payload.completedFiles,
      0,
      undefined
    );
    refreshFiles();
  });

  const unlistenCompleted = await listen<FolderCompletedEventPayload>("folder-completed", (event) => {
    useFolderTransferStore.getState().updateFolderState(
      event.payload.folderTransferId,
      folderStatusToTransferState(event.payload.status)
    );
    refreshFiles();
  });

  const unlistenTransferProgress = await listen("transfer-progress", () => {
    refreshFiles();
  });

  const unlistenTransferState = await listen("transfer-state-changed", () => {
    refreshFiles();
  });

  return () => {
    unlistenOffer();
    unlistenProgress();
    unlistenCompleted();
    unlistenTransferProgress();
    unlistenTransferState();
  };
}
