import { create } from 'zustand';
import { listen } from '@tauri-apps/api/event';
import { Peer } from '../types/peer';

interface PeerStore {
  peers: Peer[];
  setupListeners: () => Promise<() => void>;
}

export const usePeerStore = create<PeerStore>((set) => ({
  peers: [],
  setupListeners: async () => {
    const unlistenDiscovered = await listen<Peer>('peer-discovered', (event) => {
      set((state) => {
        const peer = event.payload;
        if (state.peers.some((p) => p.id === peer.id)) {
          return {
            peers: state.peers.map((p) =>
              p.id === peer.id ? { ...p, ...peer, warning: undefined } : p
            ),
          };
        }
        return { peers: [...state.peers, peer] };
      });
    });

    const unlistenLost = await listen<{ id: string }>('peer-lost', (event) => {
      set((state) => ({
        peers: state.peers.map((p) =>
          p.id === event.payload.id ? { ...p, status: 'offline' as const } : p
        ),
      }));
    });

    const unlistenIncompatible = await listen<{ id: string; reason: string }>(
      'peer-incompatible',
      (event) => {
        set((state) => {
          const nextPeer = {
            id: event.payload.id,
            name:
              state.peers.find((peer) => peer.id === event.payload.id)?.name ?? event.payload.id,
            status: 'incompatible' as const,
            warning: event.payload.reason,
          };

          if (state.peers.some((peer) => peer.id === event.payload.id)) {
            return {
              peers: state.peers.map((peer) =>
                peer.id === event.payload.id
                  ? { ...peer, status: 'incompatible', warning: event.payload.reason }
                  : peer
              ),
            };
          }

          return {
            peers: [...state.peers, nextPeer],
          };
        });
      }
    );

    return () => {
      unlistenDiscovered();
      unlistenLost();
      unlistenIncompatible();
    };
  },
}));
