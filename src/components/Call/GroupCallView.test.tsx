import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import i18n from '../../i18n/i18n';
import { GroupCallView } from './GroupCallView';
import { WebRTCCallSession } from '../../services/webrtc';

describe('GroupCallView', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en');
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('renders correctly with 1 remote participant', () => {
    const onEnd = vi.fn();
    const session: WebRTCCallSession = {
      callId: 'call-1',
      peerId: 'peer-1',
      callType: 'video',
      direction: 'incoming',
      status: 'connected',
      localStream: null,
      remoteStream: null,
      mode: 'group',
      participants: [
        {
          peerId: 'remote-1',
          status: 'connected',
          remoteStream: null,
        },
      ],
    };

    render(<GroupCallView session={session} onEnd={onEnd} />);

    expect(screen.getByTestId('group-call-view')).toBeInTheDocument();
    expect(screen.getByTestId('local-participant')).toBeInTheDocument();
    expect(screen.getByTestId('remote-participant-remote-1')).toBeInTheDocument();
    expect(screen.getByTestId('participant-badge-You')).toBeInTheDocument();
    expect(screen.getByTestId('participant-badge-remote-1')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-end'));
    expect(onEnd).toHaveBeenCalled();
  });

  it('renders correctly with 3 remote participants', () => {
    const onEnd = vi.fn();
    const session: WebRTCCallSession = {
      callId: 'call-1',
      peerId: 'peer-1',
      callType: 'audio',
      direction: 'incoming',
      status: 'connected',
      localStream: null,
      remoteStream: null,
      mode: 'group',
      participants: [
        { peerId: 'remote-1', status: 'connected', remoteStream: null },
        { peerId: 'remote-2', status: 'connected', remoteStream: null },
        { peerId: 'remote-3', status: 'connected', remoteStream: null },
      ],
    };

    render(<GroupCallView session={session} onEnd={onEnd} />);

    expect(screen.getByTestId('group-call-view')).toBeInTheDocument();
    expect(screen.getByTestId('local-participant')).toBeInTheDocument();
    expect(screen.getByTestId('remote-participant-remote-1')).toBeInTheDocument();
    expect(screen.getByTestId('remote-participant-remote-2')).toBeInTheDocument();
    expect(screen.getByTestId('remote-participant-remote-3')).toBeInTheDocument();
  });
});
