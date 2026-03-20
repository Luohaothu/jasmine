import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { Peer } from "../types/peer";

interface PeerStore {
  peers: Peer[];
  setupListeners: () => Promise<() => void>;
}

export const usePeerStore = create<PeerStore>((set) => ({
  peers: [],
  setupListeners: async () => {
    const unlistenDiscovered = await listen<Peer>("peer-discovered", (event) => {
      set((state) => {
        const peer = event.payload;
        if (state.peers.some((p) => p.id === peer.id)) {
          return { peers: state.peers.map((p) => (p.id === peer.id ? peer : p)) };
        }
        return { peers: [...state.peers, peer] };
      });
    });

    const unlistenLost = await listen<{ id: string }>("peer-lost", (event) => {
      set((state) => ({
        peers: state.peers.filter((p) => p.id !== event.payload.id),
      }));
    });

    return () => {
      unlistenDiscovered();
      unlistenLost();
    };
  },
}));
