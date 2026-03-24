import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import '../../../i18n/i18n';
import { ChatView } from '../ChatView';
import { useChatStore } from '../../../stores/chatStore';
import { usePeerStore } from '../../../stores/peerStore';
import * as tauriEvent from '@tauri-apps/api/event';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

window.HTMLElement.prototype.scrollIntoView = vi.fn();

describe('ChatView.status', () => {
  let mockUnlisten: any;

  beforeEach(() => {
    useChatStore.setState({ messages: {} });
    usePeerStore.setState({ peers: [{ id: 'peer-1', name: 'Alice', status: 'online' }] });
    mockUnlisten = vi.fn();
    vi.mocked(tauriEvent.listen).mockResolvedValue(mockUnlisten);
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  const renderWithRouter = (peerId: string) => {
    return render(
      <MemoryRouter initialEntries={[`/chat/${peerId}`]}>
        <Routes>
          <Route path="/chat/:peerId" element={<ChatView />} />
        </Routes>
      </MemoryRouter>
    );
  };

  it('listens to message-received and adds message to store', async () => {
    renderWithRouter('peer-1');

    expect(tauriEvent.listen).toHaveBeenCalledWith('message-received', expect.any(Function));

    const listenCall = vi
      .mocked(tauriEvent.listen)
      .mock.calls.find((call) => call[0] === 'message-received');
    const callback = listenCall?.[1] as Function;

    act(() => {
      callback({
        payload: {
          id: 'msg-event',
          senderId: 'peer-1',
          receiverId: 'local',
          content: 'Event Message',
          timestamp: 3000,
          status: 'delivered',
        },
      });
    });

    expect(screen.getByText('Event Message')).toBeInTheDocument();
  });

  it('cleans up listener on unmount', async () => {
    const { unmount } = renderWithRouter('peer-1');
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    unmount();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(mockUnlisten).toHaveBeenCalled();
  });
});
