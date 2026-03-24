import { invoke } from '@tauri-apps/api/core';

export interface WebRTCSupport {
  supported: boolean;
  getUserMedia: boolean;
  peerConnection: boolean;
  displayMedia: boolean;
}

export interface WebRTCPlatformInfo {
  platform: string;
  webview: string;
  webviewVersion: string | null;
}

function hasMediaDevices(): MediaDevices | undefined {
  if (typeof navigator === 'undefined') {
    return undefined;
  }

  return navigator.mediaDevices;
}

function supportsPeerConnection(): boolean {
  if (typeof RTCPeerConnection !== 'function') {
    return false;
  }

  try {
    const peerConnection = new RTCPeerConnection();
    peerConnection.close();
    return true;
  } catch {
    return false;
  }
}

export function checkWebRTCSupport(): WebRTCSupport {
  const mediaDevices = hasMediaDevices();
  const getUserMedia = typeof mediaDevices?.getUserMedia === 'function';
  const displayMedia = typeof mediaDevices?.getDisplayMedia === 'function';
  const peerConnection = supportsPeerConnection();

  return {
    supported: getUserMedia && peerConnection,
    getUserMedia,
    peerConnection,
    displayMedia,
  };
}

export async function getWebRTCPlatformInfo(): Promise<WebRTCPlatformInfo> {
  return invoke<WebRTCPlatformInfo>('get_webrtc_platform_info');
}
