import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

export type TransferState = "queued" | "active" | "completed" | "failed" | "cancelled" | "rejected" | "partially-failed";

export interface Transfer {
  id: string;
  filename: string;
  size: number;
  progress: number;
  speed: number;
  state: TransferState;
  senderId?: string;
}

export interface FileOffer {
  id: string;
  filename: string;
  size: number;
  senderId: string;
}

/* eslint-disable no-unused-vars */
interface TransferStore {
  transfers: Transfer[];
  activeOffers: FileOffer[];
  addTransfer: (transfer: Transfer) => void;
  updateProgress: (id: string, progress: number, speed: number) => void;
  updateState: (id: string, state: TransferState) => void;
  cancelTransfer: (id: string) => void;
  addOffer: (offer: FileOffer) => void;
  removeOffer: (id: string) => void;
}
/* eslint-enable no-unused-vars */

export const useTransferStore = create<TransferStore>((set) => ({
  transfers: [],
  activeOffers: [],
  addTransfer: (transfer) => set((state) => ({ transfers: [...state.transfers, transfer] })),
  updateProgress: (id, progress, speed) => set((state) => ({
    transfers: state.transfers.map(t => t.id === id ? { ...t, progress, speed } : t)
  })),
  updateState: (id, transferState) => set((state) => ({
    transfers: state.transfers.map(t => t.id === id ? { ...t, state: transferState } : t)
  })),
  cancelTransfer: (id) => set((state) => ({
    transfers: state.transfers.map(t => t.id === id ? { ...t, state: "failed" } : t)
  })),
  addOffer: (offer) => set((state) => ({ activeOffers: [...state.activeOffers, offer] })),
  removeOffer: (id) => set((state) => ({ activeOffers: state.activeOffers.filter(o => o.id !== id) })),
}));

export async function setupTransferListeners(): Promise<() => void> {
  const unlistenOffer = await listen<FileOffer>("file-offer-received", (event) => {
    useTransferStore.getState().addOffer(event.payload);
  });

  const unlistenProgress = await listen<{id: string, progress: number, speed: number}>("transfer-progress", (event) => {
    useTransferStore.getState().updateProgress(event.payload.id, event.payload.progress, event.payload.speed);
  });

  const unlistenState = await listen<{id: string, state: TransferState}>("transfer-state-changed", (event) => {
    useTransferStore.getState().updateState(event.payload.id, event.payload.state);
  });

  return () => {
    unlistenOffer();
    unlistenProgress();
    unlistenState();
  };
}
