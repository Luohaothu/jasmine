import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import i18n from '../../i18n/i18n';
import { IncomingCallOverlay } from './IncomingCallOverlay';
import { OutgoingCallOverlay } from './OutgoingCallOverlay';
import { InCallView } from './InCallView';
import { CallControlBar } from './CallControlBar';
import { WebRTCCallSession } from '../../services/webrtc';

describe('Call Components', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en');
    vi.spyOn(console, 'error').mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });
  it('renders IncomingCallOverlay and handles actions', () => {
    const onAccept = vi.fn();
    const onReject = vi.fn();
    render(<IncomingCallOverlay peerId="caller-123" onAccept={onAccept} onReject={onReject} />);

    expect(screen.getByTestId('incoming-call-overlay')).toBeInTheDocument();
    expect(screen.getByText('caller-123')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-accept'));
    expect(onAccept).toHaveBeenCalled();

    fireEvent.click(screen.getByTestId('btn-reject'));
    expect(onReject).toHaveBeenCalled();
  });

  it('renders OutgoingCallOverlay', () => {
    const onEnd = vi.fn();
    render(<OutgoingCallOverlay peerId="callee-456" status="new" onEnd={onEnd} />);

    expect(screen.getByTestId('outgoing-call-overlay')).toBeInTheDocument();
    expect(screen.getByText('callee-456')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-end'));
    expect(onEnd).toHaveBeenCalled();
  });

  it('renders InCallView with controls', () => {
    const onEnd = vi.fn();
    const session: WebRTCCallSession = {
      callId: 'call-1',
      peerId: 'peer-1',
      callType: 'audio',
      direction: 'incoming',
      status: 'connected',
      localStream: null,
      remoteStream: null,
    };

    render(<InCallView session={session} onEnd={onEnd} />);

    expect(screen.getByTestId('in-call-view')).toBeInTheDocument();
    expect(screen.getByText('peer-1')).toBeInTheDocument();
    expect(screen.getByTestId('call-control-bar')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-end'));
    expect(onEnd).toHaveBeenCalled();
  });

  it('renders CallControlBar with toggleable states', () => {
    const onMuteAudio = vi.fn();
    const onEnd = vi.fn();

    render(<CallControlBar onEnd={onEnd} onMuteAudio={onMuteAudio} isAudioMuted={false} />);

    expect(screen.getByTestId('call-control-bar')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('btn-mute'));
    expect(onMuteAudio).toHaveBeenCalledWith(true);
  });

  it('renders CallControlBar with device dropdowns and handles selection', () => {
    const onAudioDeviceSelect = vi.fn();
    const onVideoDeviceSelect = vi.fn();

    const devices: MediaDeviceInfo[] = [
      { deviceId: 'audio-1', kind: 'audioinput', label: 'Mic 1', groupId: 'g1', toJSON: () => {} },
      { deviceId: 'video-1', kind: 'videoinput', label: 'Cam 1', groupId: 'g2', toJSON: () => {} },
    ];

    render(
      <CallControlBar
        onEnd={vi.fn()}
        isVideoCall={true}
        devices={devices}
        onAudioDeviceSelect={onAudioDeviceSelect}
        onVideoDeviceSelect={onVideoDeviceSelect}
        selectedAudioDeviceId="audio-1"
        selectedVideoDeviceId="video-1"
      />
    );

    const audioSelect = screen.getByTestId('audio-device-select');
    const videoSelect = screen.getByTestId('video-device-select');

    expect(audioSelect).toBeInTheDocument();
    expect(videoSelect).toBeInTheDocument();

    fireEvent.change(audioSelect, { target: { value: 'audio-1' } });
    expect(onAudioDeviceSelect).toHaveBeenCalledWith('audio-1');

    fireEvent.change(videoSelect, { target: { value: 'video-1' } });
    expect(onVideoDeviceSelect).toHaveBeenCalledWith('video-1');
  });
});
