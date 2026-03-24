import React from 'react';
import { useTranslation } from 'react-i18next';
import { MicIcon, MicOffIcon, VideoIcon, VideoOffIcon, MonitorIcon, PhoneOffIcon } from './icons';
import styles from './Call.module.css';

export interface CallControlBarProps {
  onEnd: () => void;
  onMuteAudio?: (mute: boolean) => void;
  onMuteVideo?: (mute: boolean) => void;
  isAudioMuted?: boolean;
  isVideoMuted?: boolean;
  isVideoCall?: boolean;
  isScreenSharing?: boolean;
  onToggleScreenShare?: () => void;
  devices?: MediaDeviceInfo[];
  selectedAudioDeviceId?: string | null;
  selectedVideoDeviceId?: string | null;
  onAudioDeviceSelect?: (deviceId: string) => void;
  onVideoDeviceSelect?: (deviceId: string) => void;
}

export const CallControlBar: React.FC<CallControlBarProps> = ({
  onEnd,
  onMuteAudio,
  onMuteVideo,
  isAudioMuted = false,
  isVideoMuted = false,
  isVideoCall = false,
  isScreenSharing = false,
  onToggleScreenShare,
  devices = [],
  selectedAudioDeviceId,
  selectedVideoDeviceId,
  onAudioDeviceSelect,
  onVideoDeviceSelect,
}) => {
  const { t } = useTranslation();

  return (
    <div className={styles.controls} data-testid="call-control-bar">
      <div className={styles.deviceSelectGroup}>
        {onAudioDeviceSelect && devices.filter((d) => d.kind === 'audioinput').length > 0 && (
          <select
            className={styles.deviceSelect}
            value={selectedAudioDeviceId || ''}
            onChange={(e) => onAudioDeviceSelect(e.target.value)}
            data-testid="audio-device-select"
            aria-label={t('call.settings.audioInput')}
          >
            <option value="" disabled>
              {t('call.settings.selectAudio')}
            </option>
            {devices
              .filter((d) => d.kind === 'audioinput')
              .map((d) => (
                <option key={d.deviceId} value={d.deviceId}>
                  {d.label || t('call.settings.microphoneFallback', { id: d.deviceId.slice(0, 5) })}
                </option>
              ))}
          </select>
        )}
      </div>

      <button
        type="button"
        className={styles.btn}
        onClick={() => onMuteAudio?.(!isAudioMuted)}
        aria-label={isAudioMuted ? t('call.action.unmute') : t('call.action.mute')}
        data-testid="btn-mute"
      >
        {isAudioMuted ? (
          <MicOffIcon className={styles.icon} />
        ) : (
          <MicIcon className={styles.icon} />
        )}
      </button>

      {isVideoCall && (
        <>
          <button
            type="button"
            className={styles.btn}
            onClick={() => onMuteVideo?.(!isVideoMuted)}
            aria-label={isVideoMuted ? t('call.action.startVideo') : t('call.action.stopVideo')}
            data-testid="btn-video"
          >
            {isVideoMuted ? (
              <VideoOffIcon className={styles.icon} />
            ) : (
              <VideoIcon className={styles.icon} />
            )}
          </button>

          <div className={styles.deviceSelectGroup}>
            {onVideoDeviceSelect && devices.filter((d) => d.kind === 'videoinput').length > 0 && (
              <select
                className={styles.deviceSelect}
                value={selectedVideoDeviceId || ''}
                onChange={(e) => onVideoDeviceSelect(e.target.value)}
                data-testid="video-device-select"
                aria-label={t('call.settings.videoInput')}
              >
                <option value="" disabled>
                  {t('call.settings.selectVideo')}
                </option>
                {devices
                  .filter((d) => d.kind === 'videoinput')
                  .map((d) => (
                    <option key={d.deviceId} value={d.deviceId}>
                      {d.label || t('call.settings.cameraFallback', { id: d.deviceId.slice(0, 5) })}
                    </option>
                  ))}
              </select>
            )}
          </div>
        </>
      )}

      <button
        type="button"
        className={`${styles.btn} ${isScreenSharing ? styles.btnActive : ''}`}
        onClick={onToggleScreenShare}
        title={isScreenSharing ? t('call.action.stopScreenShare') : t('call.action.screenShare')}
        aria-label={
          isScreenSharing ? t('call.action.stopScreenShare') : t('call.action.screenShare')
        }
        data-testid="btn-screen-share"
      >
        <MonitorIcon className={styles.icon} />
      </button>

      <button
        type="button"
        className={`${styles.btn} ${styles.btnReject}`}
        onClick={onEnd}
        aria-label={t('call.action.end')}
        data-testid="btn-end"
      >
        <PhoneOffIcon className={styles.icon} />
      </button>
    </div>
  );
};
