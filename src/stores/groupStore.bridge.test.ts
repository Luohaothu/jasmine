import { beforeEach, describe, expect, it, vi } from 'vitest';

import { setupGroupListeners, useGroupStore } from './groupStore';

const { listeners, invokeMock } = vi.hoisted(() => ({
  listeners: new Map<string, Function>(),
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (eventName: string, callback: Function) => {
    listeners.set(eventName, callback);
    return () => {
      listeners.delete(eventName);
    };
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

function flushTasks(): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
}

describe('groupStore bridge listeners', () => {
  beforeEach(() => {
    listeners.clear();
    invokeMock.mockReset();
    useGroupStore.setState({
      groups: {
        'group-1': {
          id: 'group-1',
          name: 'Study Group',
          members: [
            { id: 'local', name: 'Local Device', status: 'online' },
            { id: 'peer-1', name: 'Alice', status: 'online' },
          ],
          messages: [],
          createdAt: 100,
        },
      },
    });
  });

  it('registers all group listeners and cleanup unregisters them', async () => {
    const cleanup = await setupGroupListeners();

    expect(Array.from(listeners.keys()).sort()).toEqual([
      'group-created',
      'group-member-added',
      'group-member-removed',
      'group-message-received',
      'group-updated',
    ]);

    cleanup();

    expect(listeners.size).toBe(0);
  });

  it('appends mapped group messages from bridge payloads', async () => {
    await setupGroupListeners();

    listeners.get('group-message-received')?.({
      payload: {
        groupId: 'group-1',
        id: 'msg-1',
        senderId: 'peer-1',
        senderName: 'Alice',
        content: 'Hello group',
        timestamp: 123,
        encrypted: true,
        isOwn: false,
        status: 'delivered',
        replyToId: 'reply-1',
        replyToPreview: 'Earlier message',
      },
    });

    expect(useGroupStore.getState().groups['group-1']?.messages).toEqual([
      {
        id: 'msg-1',
        senderId: 'peer-1',
        senderName: 'Alice',
        content: 'Hello group',
        timestamp: 123,
        encrypted: true,
        isOwn: false,
        status: 'delivered',
        replyToId: 'reply-1',
        replyToPreview: 'Earlier message',
      },
    ]);
  });

  it('refreshes group state for created, updated, and membership events', async () => {
    invokeMock
      .mockResolvedValueOnce({
        id: 'group-2',
        name: 'Created Group',
        members: [{ id: 'peer-2', name: 'Bob', status: 'online' }],
        createdAt: 200,
      })
      .mockResolvedValueOnce({
        id: 'group-1',
        name: 'Renamed Group',
        members: [{ id: 'peer-1', name: 'Alice', status: 'online' }],
        createdAt: 100,
      })
      .mockResolvedValueOnce({
        id: 'group-1',
        name: 'Renamed Group',
        members: [
          { id: 'peer-1', name: 'Alice', status: 'online' },
          { id: 'peer-3', name: 'Cara', status: 'offline' },
        ],
        createdAt: 100,
      })
      .mockResolvedValueOnce({
        id: 'group-1',
        name: 'Renamed Group',
        members: [{ id: 'peer-3', name: 'Cara', status: 'offline' }],
        createdAt: 100,
      });

    await setupGroupListeners();

    listeners.get('group-created')?.({ payload: { groupId: 'group-2' } });
    await flushTasks();
    expect(useGroupStore.getState().groups['group-2']).toEqual({
      id: 'group-2',
      name: 'Created Group',
      members: [{ id: 'peer-2', name: 'Bob', status: 'online' }],
      messages: [],
      createdAt: 200,
    });

    listeners.get('group-updated')?.({ payload: { groupId: 'group-1' } });
    await flushTasks();
    expect(useGroupStore.getState().groups['group-1']?.name).toBe('Renamed Group');
    expect(useGroupStore.getState().groups['group-1']?.messages).toEqual([]);

    listeners.get('group-member-added')?.({
      payload: { groupId: 'group-1', memberIds: ['peer-3'] },
    });
    await flushTasks();
    expect(useGroupStore.getState().groups['group-1']?.members).toEqual([
      { id: 'peer-1', name: 'Alice', status: 'online' },
      { id: 'peer-3', name: 'Cara', status: 'offline' },
    ]);

    listeners.get('group-member-removed')?.({
      payload: { groupId: 'group-1', memberIds: ['peer-1'] },
    });
    await flushTasks();
    expect(useGroupStore.getState().groups['group-1']?.members).toEqual([
      { id: 'peer-3', name: 'Cara', status: 'offline' },
    ]);

    expect(invokeMock).toHaveBeenNthCalledWith(1, 'get_group_info', { groupId: 'group-2' });
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'get_group_info', { groupId: 'group-1' });
    expect(invokeMock).toHaveBeenNthCalledWith(3, 'get_group_info', { groupId: 'group-1' });
    expect(invokeMock).toHaveBeenNthCalledWith(4, 'get_group_info', { groupId: 'group-1' });
  });
});
