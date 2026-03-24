import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ChatHeader } from '../ChatHeader';
import i18n from '../../../i18n/i18n';
import * as useCallSupportModule from '../../../hooks/useCallSupport';

vi.mock('../../../hooks/useCallSupport', () => ({
  useCallSupport: vi.fn(),
}));

describe('ChatHeader', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en');
    vi.mocked(useCallSupportModule.useCallSupport).mockReturnValue({
      supported: true,
      platform: 'linux',
      loading: false,
    });
  });

  it('renders peer name and status', () => {
    render(<ChatHeader peerName="Alice" status="online" />);
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('online')).toBeInTheDocument();
  });

  it('renders offline status correctly', () => {
    render(<ChatHeader peerName="Bob" status="offline" />);
    expect(screen.getByText('Bob')).toBeInTheDocument();
    expect(screen.getByText('offline')).toBeInTheDocument();
  });

  it('renders back button when onBack is provided', () => {
    let clicked = false;
    render(
      <ChatHeader
        peerName="Charlie"
        status="online"
        onBack={() => {
          clicked = true;
        }}
      />
    );
    const backBtn = screen.getByRole('button', { name: /back/i });
    backBtn.click();
    expect(clicked).toBe(true);
  });

  it('renders call buttons when supported is true and handlers are provided', () => {
    render(
      <ChatHeader peerName="Dave" status="online" onAudioCall={() => {}} onVideoCall={() => {}} />
    );
    expect(screen.getByRole('button', { name: /Audio Call/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Video Call/i })).toBeInTheDocument();
  });

  it('hides call buttons when supported is false', () => {
    vi.mocked(useCallSupportModule.useCallSupport).mockReturnValue({
      supported: false,
      platform: 'linux',
      loading: false,
    });
    render(
      <ChatHeader peerName="Eve" status="online" onAudioCall={() => {}} onVideoCall={() => {}} />
    );
    expect(screen.queryByRole('button', { name: /Audio Call/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Video Call/i })).not.toBeInTheDocument();
  });
});
