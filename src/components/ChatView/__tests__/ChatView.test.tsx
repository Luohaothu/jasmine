import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import '../../../i18n/i18n';
import { ChatView } from '../ChatView';
import { useChatStore } from '../../../stores/chatStore';
import { usePeerStore } from '../../../stores/peerStore';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

window.HTMLElement.prototype.scrollIntoView = vi.fn();

describe('ChatView', () => {
  beforeEach(() => {
    useChatStore.setState({ messages: {} });
    usePeerStore.setState({ peers: [{ id: 'peer-1', name: 'Alice', status: 'online' }] });
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

  it('renders empty state when no messages', () => {
    renderWithRouter('peer-1');
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('Send the first message to start chatting')).toBeInTheDocument();
  });

  it('renders peer as offline if not found in store', () => {
    renderWithRouter('unknown-peer');
    expect(screen.getByText('Unknown')).toBeInTheDocument();
    expect(screen.getByText('offline')).toBeInTheDocument();
  });
});
