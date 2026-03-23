import { beforeEach, describe, expect, it, vi } from 'vitest';

import { usePeerStore } from './peerStore';

const { listeners } = vi.hoisted(() => ({
  listeners: new Map<string, Function>(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (eventName: string, callback: Function) => {
    listeners.set(eventName, callback);
    return () => {
      listeners.delete(eventName);
    };
  }),
}));

describe('peerStore bridge listeners', () => {
  beforeEach(() => {
    listeners.clear();
    usePeerStore.setState({ peers: [] });
  });

  it('updates peer state for incompatible peer events and cleanup unregisters listeners', async () => {
    const cleanup = await usePeerStore.getState().setupListeners();

    listeners.get('peer-discovered')?.({
      payload: { id: 'peer-1', name: 'Legacy Peer', status: 'online', warning: undefined },
    });
    listeners.get('peer-incompatible')?.({
      payload: { id: 'peer-1', reason: 'Update required' },
    });

    expect(usePeerStore.getState().peers).toEqual([
      {
        id: 'peer-1',
        name: 'Legacy Peer',
        status: 'incompatible',
        warning: 'Update required',
      },
    ]);

    cleanup();
    expect(listeners.size).toBe(0);
  });
});
