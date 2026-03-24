import React, { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { CallControlBar } from './CallControlBar';
import { WebRTCCallSession } from '../../services/webrtc';
import { useCallStore } from '../../stores/callStore';
import { GroupCallView } from './GroupCallView';
import { ScreenShareIndicator } from './ScreenShareIndicator';
import { MonitorIcon } from './icons';
import styles from './Call.module.css';

export interface InCallViewProps {
  session: WebRTCCallSession;
  onEnd: () => void;
  onMuteAudio?: (mute: boolean) => void;
  onMuteVideo?: (mute: boolean) => void;
  isAudioMuted?: boolean;
  isVideoMuted?: boolean;
}

export const InCallView: React.FC<InCallViewProps> = ({
  session,
  onEnd,
  onMuteAudio: propsOnMuteAudio,
  onMuteVideo: propsOnMuteVideo,
  isAudioMuted: propsIsAudioMuted,
  isVideoMuted: propsIsVideoMuted,
}) => {
  const { t } = useTranslation();
  const localVideoRef = useRef<HTMLVideoElement>(null);
  const remoteVideoRef = useRef<HTMLVideoElement>(null);

  const isMuted = useCallStore((state) => state.isMuted);
  const isCameraOff = useCallStore((state) => state.isCameraOff);
  const isScreenSharing = useCallStore((state) => state.isScreenSharing);
  const devices = useCallStore((state) => state.devices);
  const selectedAudioDeviceId = useCallStore((state) => state.selectedAudioDeviceId);
  const selectedVideoDeviceId = useCallStore((state) => state.selectedVideoDeviceId);

  const toggleMute = useCallStore((state) => state.toggleMute);
  const toggleCamera = useCallStore((state) => state.toggleCamera);
  const toggleScreenShare = useCallStore((state) => state.toggleScreenShare);
  const setAudioDevice = useCallStore((state) => state.setAudioDevice);
  const setVideoDevice = useCallStore((state) => state.setVideoDevice);
  const enumerateDevices = useCallStore((state) => state.enumerateDevices);

  useEffect(() => {
    enumerateDevices();
  }, [enumerateDevices]);

  const isVideoCall = session.callType === 'video';

  const isAudioMuted = propsIsAudioMuted ?? isMuted;
  const isVideoMuted = propsIsVideoMuted ?? isCameraOff;
  const onMuteAudio = propsOnMuteAudio ?? toggleMute;
  const onMuteVideo = propsOnMuteVideo ?? toggleCamera;

  const isGroupCall =
    session.mode === 'group' || (session.participants && session.participants.length > 1);

  useEffect(() => {
    if (!isGroupCall && localVideoRef.current && session.localStream) {
      localVideoRef.current.srcObject = session.localStream;
    }
  }, [session.localStream, isGroupCall]);

  useEffect(() => {
    if (!isGroupCall && remoteVideoRef.current && session.remoteStream) {
      remoteVideoRef.current.srcObject = session.remoteStream;
    }
  }, [session.remoteStream, isGroupCall]);

  if (isGroupCall) {
    return (
      <GroupCallView
        session={session}
        onEnd={onEnd}
        onMuteAudio={onMuteAudio as (mute: boolean) => void}
        onMuteVideo={onMuteVideo as (mute: boolean) => void}
        isAudioMuted={isAudioMuted}
        isVideoMuted={isVideoMuted}
      />
    );
  }

  const remoteIsSharing = session.callType === 'screen';
  const isReconnecting = session.status === 'disconnected';

  return (
    <div className={styles.overlay} data-testid="in-call-view">
      {isScreenSharing && <ScreenShareIndicator onStopSharing={toggleScreenShare} />}
      {isReconnecting && (
        <div className={styles.reconnectingBanner} data-testid="reconnecting-banner">
          {t('call.status.reconnecting')}
        </div>
      )}

      {isVideoCall || remoteIsSharing ? (
        <div className={styles.videoContainer}>
          {remoteIsSharing && (
            <div className={styles.remoteScreenShareLabel}>
              <MonitorIcon className={styles.iconSmall} />
              {t('call.status.remoteScreenSharing', { name: session.peerId })}
            </div>
          )}
          <video
            ref={remoteVideoRef}
            className={styles.remoteVideo}
            autoPlay
            playsInline
            data-testid="remote-video"
          >
            <track kind="captions" />
          </video>
          <video
            ref={localVideoRef}
            className={styles.localVideo}
            autoPlay
            playsInline
            muted
            data-testid="local-video"
          >
            <track kind="captions" />
          </video>
        </div>
      ) : (
        <>
          <div className={styles.audioAvatar}>{session.peerId.charAt(0).toUpperCase()}</div>
          <div className={styles.title}>{session.peerId}</div>
          <div className={styles.status}>
            {isReconnecting ? t('call.status.reconnecting') : t('call.status.connected')}
          </div>
        </>
      )}

      <CallControlBar
        onEnd={onEnd}
        onMuteAudio={onMuteAudio as (mute: boolean) => void}
        onMuteVideo={onMuteVideo as (mute: boolean) => void}
        isAudioMuted={isAudioMuted}
        isVideoMuted={isVideoMuted}
        isVideoCall={isVideoCall || remoteIsSharing}
        isScreenSharing={isScreenSharing}
        onToggleScreenShare={toggleScreenShare}
        devices={devices}
        selectedAudioDeviceId={selectedAudioDeviceId}
        selectedVideoDeviceId={selectedVideoDeviceId}
        onAudioDeviceSelect={setAudioDevice}
        onVideoDeviceSelect={setVideoDevice}
      />
    </div>
  );
};
