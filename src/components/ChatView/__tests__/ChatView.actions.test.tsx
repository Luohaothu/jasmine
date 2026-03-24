import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import '../../../i18n/i18n';

import { invoke } from '@tauri-apps/api/core';

import { ChatView } from '../ChatView';
import { useChatStore } from '../../../stores/chatStore';
import { usePeerStore } from '../../../stores/peerStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

window.HTMLElement.prototype.scrollIntoView = vi.fn();

describe('ChatView actions', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePeerStore.setState({ peers: [{ id: 'peer-1', name: 'Alice', status: 'online' }] });
    useChatStore.setState({
      messages: {
        'peer-1': [
          {
            id: 'msg-1',
            senderId: 'local',
            receiverId: 'peer-1',
            content: 'Hello there',
            timestamp: 123,
            status: 'sent',
          },
        ],
      },
      replyingTo: null,
    });
    window.confirm = vi.fn(() => true);
  });

  const renderWithRouter = () => {
    return render(
      <MemoryRouter initialEntries={['/chat/peer-1']}>
        <Routes>
          <Route path="/chat/:peerId" element={<ChatView />} />
        </Routes>
      </MemoryRouter>
    );
  };

  it('invokes edit_message and delete_message for own message actions', () => {
    renderWithRouter();

    fireEvent.click(screen.getByText('Edit'));
    fireEvent.change(screen.getByDisplayValue('Hello there'), {
      target: { value: 'Updated copy' },
    });
    fireEvent.click(screen.getByText('Save'));

    expect(invoke).toHaveBeenCalledWith('edit_message', {
      messageId: 'msg-1',
      newContent: 'Updated copy',
    });

    fireEvent.click(screen.getByText('Delete'));

    expect(invoke).toHaveBeenCalledWith('delete_message', {
      messageId: 'msg-1',
    });
  });
});
