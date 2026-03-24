import React, { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { WebRTCCallSession, CallParticipant } from '../../services/webrtc';
import { CallControlBar } from './CallControlBar';
import { ParticipantBadge } from './ParticipantBadge';
import { useCallStore } from '../../stores/callStore';
import { ScreenShareIndicator } from './ScreenShareIndicator';
import { MonitorIcon } from './icons';
import styles from './Call.module.css';

export interface GroupCallViewProps {
  session: WebRTCCallSession;
  onEnd: () => void;
  onMuteAudio?: (mute: boolean) => void;
  onMuteVideo?: (mute: boolean) => void;
  isAudioMuted?: boolean;
  isVideoMuted?: boolean;
}

const RemoteParticipant: React.FC<{
  participant: CallParticipant;
  isVideoCall: boolean;
  isScreenSharing: boolean;
}> = ({ participant, isVideoCall, isScreenSharing }) => {
  const videoRef = useRef<HTMLVideoElement>(null);

  useEffect(() => {
    if (videoRef.current && participant.remoteStream) {
      videoRef.current.srcObject = participant.remoteStream;
    }
  }, [participant.remoteStream]);

  return (
    <div className={styles.participantBox} data-testid={`remote-participant-${participant.peerId}`}>
      {isScreenSharing && (
        <div className={styles.remoteScreenShareLabel}>
          <MonitorIcon className={styles.iconSmall} />
        </div>
      )}
      {isVideoCall && participant.remoteStream ? (
        <video
          ref={videoRef}
          className={styles.participantVideo}
          autoPlay
          playsInline
          data-testid={`video-${participant.peerId}`}
        >
          <track kind="captions" />
        </video>
      ) : (
        <div className={styles.participantFallback}>
          {participant.peerId.charAt(0).toUpperCase()}
        </div>
      )}
      <ParticipantBadge name={participant.peerId} status={participant.status} isMuted={false} />
    </div>
  );
};

export const GroupCallView: React.FC<GroupCallViewProps> = ({
  session,
  onEnd,
  onMuteAudio: propsOnMuteAudio,
  onMuteVideo: propsOnMuteVideo,
  isAudioMuted: propsIsAudioMuted,
  isVideoMuted: propsIsVideoMuted,
}) => {
  const { t } = useTranslation();
  const localVideoRef = useRef<HTMLVideoElement>(null);

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

  useEffect(() => {
    if (localVideoRef.current && session.localStream) {
      localVideoRef.current.srcObject = session.localStream;
    }
  }, [session.localStream]);

  const isVideoCall = session.callType === 'video';
  const isAudioMuted = propsIsAudioMuted ?? isMuted;
  const isVideoMuted = propsIsVideoMuted ?? isCameraOff;
  const onMuteAudio = propsOnMuteAudio ?? toggleMute;
  const onMuteVideo = propsOnMuteVideo ?? toggleCamera;

  const participants = session.participants || [];
  const totalGridCount = 1 + participants.length;
  const localParticipantLabel = t('chat.you');

  const displayCount = Math.min(totalGridCount, 4);
  const gridClass = styles[`gridCount${displayCount}`] || styles.gridCount4;

  return (
    <div className={styles.overlay} data-testid="group-call-view">
      {isScreenSharing && <ScreenShareIndicator onStopSharing={toggleScreenShare} />}

      <div className={`${styles.groupGrid} ${gridClass}`}>
        <div className={styles.participantBox} data-testid="local-participant">
          {isVideoCall && !isVideoMuted && session.localStream ? (
            <video
              ref={localVideoRef}
              className={styles.participantVideo}
              autoPlay
              playsInline
              muted
              data-testid="local-video"
            >
              <track kind="captions" />
            </video>
          ) : (
            <div className={styles.participantFallback}>{localParticipantLabel}</div>
          )}
          <ParticipantBadge
            name={localParticipantLabel}
            status="connected"
            isMuted={isAudioMuted}
          />
        </div>

        {participants.slice(0, 3).map((p) => (
          <RemoteParticipant
            key={p.peerId}
            participant={p}
            isVideoCall={isVideoCall || session.callType === 'screen'}
            isScreenSharing={session.callType === 'screen' && session.peerId === p.peerId}
          />
        ))}
      </div>

      <CallControlBar
        onEnd={onEnd}
        onMuteAudio={onMuteAudio as (mute: boolean) => void}
        onMuteVideo={onMuteVideo as (mute: boolean) => void}
        isAudioMuted={isAudioMuted}
        isVideoMuted={isVideoMuted}
        isVideoCall={isVideoCall || session.callType === 'screen'}
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
