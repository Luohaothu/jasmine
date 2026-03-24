import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { usePeerStore } from '../stores/peerStore';

const {
  isPermissionGrantedMock,
  listenMock,
  listeners,
  requestPermissionMock,
  sendNotificationMock,
} = vi.hoisted(() => ({
  isPermissionGrantedMock: vi.fn(),
  listenMock: vi.fn(),
  listeners: new Map<string, Function>(),
  requestPermissionMock: vi.fn(),
  sendNotificationMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: listenMock,
}));

vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: isPermissionGrantedMock,
  requestPermission: requestPermissionMock,
  sendNotification: sendNotificationMock,
}));

import { useNotifications } from './useNotifications';

async function flushTasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe('useNotifications', () => {
  let hasFocus = true;

  beforeEach(() => {
    listeners.clear();
    listenMock.mockReset();
    isPermissionGrantedMock.mockReset();
    requestPermissionMock.mockReset();
    sendNotificationMock.mockReset();

    listenMock.mockImplementation(async (eventName: string, callback: Function) => {
      listeners.set(eventName, callback);
      return () => {
        listeners.delete(eventName);
      };
    });

    hasFocus = true;
    vi.spyOn(document, 'hasFocus').mockImplementation(() => hasFocus);

    usePeerStore.setState({
      peers: [{ id: 'peer-1', name: 'Alice', status: 'online' }],
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('notifies for unfocused direct messages and suppresses duplicate mention events', async () => {
    hasFocus = false;
    isPermissionGrantedMock.mockResolvedValue(false);
    requestPermissionMock.mockResolvedValue('granted');

    const { unmount } = renderHook(() => useNotifications());

    await waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith('message-received', expect.any(Function));
    });

    listeners.get('message-received')?.({
      payload: {
        id: 'msg-1',
        senderId: 'peer-1',
        receiverId: 'local',
        content: 'Hello from Alice',
        timestamp: 123,
        status: 'delivered',
      },
    });

    await waitFor(() => {
      expect(requestPermissionMock).toHaveBeenCalledTimes(1);
      expect(sendNotificationMock).toHaveBeenCalledWith({
        title: 'Alice',
        body: 'Hello from Alice',
      });
    });

    window.dispatchEvent(
      new CustomEvent('jasmine:mention-received', {
        detail: {
          messageId: 'msg-1',
          mentionedUserId: 'local',
          senderName: 'Alice',
        },
      })
    );

    await flushTasks();
    expect(sendNotificationMock).toHaveBeenCalledTimes(1);

    unmount();
  });

  it('does not notify while focused, for own messages, or ordinary group traffic', async () => {
    isPermissionGrantedMock.mockResolvedValue(true);

    const { unmount } = renderHook(() => useNotifications());

    await waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith('message-received', expect.any(Function));
    });

    listeners.get('message-received')?.({
      payload: {
        id: 'focused-msg',
        senderId: 'peer-1',
        receiverId: 'local',
        content: 'You can already see this',
        timestamp: 456,
        status: 'delivered',
      },
    });

    await flushTasks();
    expect(sendNotificationMock).not.toHaveBeenCalled();

    hasFocus = false;
    window.dispatchEvent(new Event('blur'));

    listeners.get('message-received')?.({
      payload: {
        id: 'own-msg',
        senderId: 'local',
        receiverId: 'peer-1',
        content: 'My own message',
        timestamp: 789,
        status: 'sent',
      },
    });

    window.dispatchEvent(
      new CustomEvent('group-message-received', {
        detail: {
          id: 'group-msg',
          senderId: 'peer-2',
          content: 'Ordinary group chatter',
        },
      })
    );

    await flushTasks();
    expect(sendNotificationMock).not.toHaveBeenCalled();
    expect(listenMock).not.toHaveBeenCalledWith('group-message-received', expect.any(Function));

    unmount();
  });

  it('notifies for unfocused mentions', async () => {
    hasFocus = false;
    isPermissionGrantedMock.mockResolvedValue(true);

    const { unmount } = renderHook(() => useNotifications());

    await waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith('message-received', expect.any(Function));
    });

    window.dispatchEvent(
      new CustomEvent('jasmine:mention-received', {
        detail: {
          messageId: 'group-msg-1',
          mentionedUserId: 'local',
          senderName: 'Bob',
        },
      })
    );

    await waitFor(() => {
      expect(sendNotificationMock).toHaveBeenCalledWith({
        title: 'Bob mentioned you',
        body: 'Open Jasmine to view the message.',
      });
    });

    unmount();
  });

  it('gracefully skips notifications when permission is denied', async () => {
    hasFocus = false;
    isPermissionGrantedMock.mockResolvedValue(false);
    requestPermissionMock.mockResolvedValue('denied');

    const { unmount } = renderHook(() => useNotifications());

    await waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith('message-received', expect.any(Function));
    });

    listeners.get('message-received')?.({
      payload: {
        id: 'msg-denied',
        senderId: 'peer-1',
        receiverId: 'local',
        content: 'Permission denied test',
        timestamp: 999,
        status: 'delivered',
      },
    });

    await flushTasks();
    expect(requestPermissionMock).toHaveBeenCalledTimes(1);
    expect(sendNotificationMock).not.toHaveBeenCalled();

    unmount();
  });
});
