import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { checkWebRTCSupport, getWebRTCPlatformInfo } from './webrtc-support';

const navigatorDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
const peerConnectionDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'RTCPeerConnection');

type MediaDevicesLike = Pick<MediaDevices, 'getUserMedia' | 'getDisplayMedia'>;

function setNavigatorMediaDevices(mediaDevices?: Partial<MediaDevicesLike>): void {
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    writable: true,
    value: mediaDevices ? { mediaDevices } : {},
  });
}

function setPeerConnection(
  ctor?: new () => {
    close: () => void;
  }
): void {
  if (ctor) {
    Object.defineProperty(globalThis, 'RTCPeerConnection', {
      configurable: true,
      writable: true,
      value: ctor,
    });
    return;
  }

  Reflect.deleteProperty(globalThis, 'RTCPeerConnection');
}

function restoreGlobals(): void {
  if (navigatorDescriptor) {
    Object.defineProperty(globalThis, 'navigator', navigatorDescriptor);
  }

  if (peerConnectionDescriptor) {
    Object.defineProperty(globalThis, 'RTCPeerConnection', peerConnectionDescriptor);
    return;
  }

  Reflect.deleteProperty(globalThis, 'RTCPeerConnection');
}

describe('checkWebRTCSupport', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    setNavigatorMediaDevices();
    setPeerConnection();
  });

  afterEach(() => {
    restoreGlobals();
  });

  it('reports supported capabilities when media APIs are available', () => {
    const closeSpy = vi.fn();

    setNavigatorMediaDevices({
      getUserMedia: vi.fn(),
      getDisplayMedia: vi.fn(),
    });

    class MockRTCPeerConnection {
      close(): void {
        closeSpy();
      }
    }

    setPeerConnection(MockRTCPeerConnection);

    expect(checkWebRTCSupport()).toEqual({
      supported: true,
      getUserMedia: true,
      peerConnection: true,
      displayMedia: true,
    });
    expect(closeSpy).toHaveBeenCalledTimes(1);
  });

  it('reports unsupported capabilities when WebRTC APIs are missing', () => {
    expect(checkWebRTCSupport()).toEqual({
      supported: false,
      getUserMedia: false,
      peerConnection: false,
      displayMedia: false,
    });
  });

  it('treats peer connection construction failures as unsupported without throwing', () => {
    setNavigatorMediaDevices({
      getUserMedia: vi.fn(),
      getDisplayMedia: vi.fn(),
    });

    class ThrowingRTCPeerConnection {
      constructor() {
        throw new Error('constructor blocked');
      }

      close(): void {}
    }

    setPeerConnection(ThrowingRTCPeerConnection);

    expect(checkWebRTCSupport()).toEqual({
      supported: false,
      getUserMedia: true,
      peerConnection: false,
      displayMedia: true,
    });
  });
});

describe('getWebRTCPlatformInfo', () => {
  afterEach(() => {
    restoreGlobals();
  });

  it('loads platform information from the Tauri command', async () => {
    const info = {
      platform: 'linux',
      webview: 'webkitgtk',
      webviewVersion: '2.44.0',
    };
    invokeMock.mockResolvedValueOnce(info);

    await expect(getWebRTCPlatformInfo()).resolves.toEqual(info);
    expect(invokeMock).toHaveBeenCalledWith('get_webrtc_platform_info');
  });
});
