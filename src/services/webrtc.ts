import { invoke } from '@tauri-apps/api/core';

import { checkWebRTCSupport, type WebRTCSupport } from '../utils/webrtc-support';

export type CallType = 'audio' | 'video' | 'screen';
export type SignalingCallType = 'Audio' | 'Video' | 'Screen';
export type CallDirection = 'incoming' | 'outgoing';
export type CallStatus = 'new' | 'connecting' | 'connected' | 'disconnected' | 'closed';
export type CallMode = 'direct' | 'group';

export const MAX_GROUP_CALL_PARTICIPANTS = 4;
export const GROUP_CALL_CAPACITY_ERROR = `Group call participant limit reached (${MAX_GROUP_CALL_PARTICIPANTS} participants max)`;
export const CALL_NO_ANSWER_TIMEOUT_MS = 30_000;
export const CALL_DISCONNECT_TIMEOUT_MS = 10_000;

const MAX_GROUP_REMOTE_PARTICIPANTS = MAX_GROUP_CALL_PARTICIPANTS - 1;
const NO_ANSWER_ERROR = 'No answer';
const CALL_DISCONNECTED_ERROR = 'Call disconnected';

type TimerHandle = ReturnType<typeof setTimeout>;

export interface SignalingIceCandidate {
  candidate: string;
  sdpMid?: string | null;
  sdpMlineIndex?: number | null;
}

export interface CallParticipant {
  peerId: string;
  status: CallStatus;
  remoteStream: MediaStream | null;
  lastError?: string;
}

export interface WebRTCCallSession {
  callId: string;
  groupId?: string;
  peerId: string;
  callType: CallType;
  direction: CallDirection;
  status: CallStatus;
  localStream: MediaStream | null;
  remoteStream: MediaStream | null;
  isScreenSharing?: boolean;
  lastError?: string;
  mode?: CallMode;
  participants?: CallParticipant[];
}

export interface WebRTCSignaling {
  sendCallJoin(groupId: string, callId: string): Promise<void>;
  sendCallLeave(callId: string): Promise<void>;
  sendCallOffer(
    peerId: string,
    callId: string,
    sdp: string,
    callType: SignalingCallType
  ): Promise<void>;
  sendCallAnswer(peerId: string, callId: string, sdp: string): Promise<void>;
  sendIceCandidate(peerId: string, callId: string, candidate: SignalingIceCandidate): Promise<void>;
  sendCallHangup(peerId: string, callId: string): Promise<void>;
}

export interface WebRTCDependencies {
  signaling: WebRTCSignaling;
  supportCheck: () => WebRTCSupport;
  getUserMedia: (constraints: MediaStreamConstraints) => Promise<MediaStream>;
  getDisplayMedia: (constraints: DisplayMediaStreamOptions) => Promise<MediaStream>;
  createPeerConnection: (configuration: RTCConfiguration) => RTCPeerConnection;
  createMediaStream: () => MediaStream;
  enumerateDevices: () => Promise<MediaDeviceInfo[]>;
  createCallId: () => string;
}

type SessionListener = (session: WebRTCCallSession) => void;

interface InternalParticipantSession extends CallParticipant {
  peerConnection: RTCPeerConnection | null;
  pendingRemoteCandidates: RTCIceCandidateInit[];
  localTracksAttached: boolean;
  remoteElement: HTMLMediaElement | null;
  cleanupInProgress: boolean;
  noAnswerTimeout: TimerHandle | null;
  disconnectTimeout: TimerHandle | null;
  renegotiationInProgress: boolean;
  renegotiationQueued: boolean;
}

interface InternalWebRTCCallSession extends Omit<
  WebRTCCallSession,
  'mode' | 'participants' | 'remoteStream' | 'isScreenSharing'
> {
  mode: CallMode;
  participants: Map<string, InternalParticipantSession>;
  localElement: HTMLMediaElement | null;
  cleanupInProgress: boolean;
  cameraTrack: MediaStreamTrack | null;
  screenShareTrack: MediaStreamTrack | null;
  isScreenSharing: boolean;
}

const RTC_CONFIGURATION: RTCConfiguration = {
  iceServers: [],
};

const SIGNALING_CALL_TYPES: Record<CallType, SignalingCallType> = {
  audio: 'Audio',
  video: 'Video',
  screen: 'Screen',
};

function normalizeCallType(callType: CallType | SignalingCallType): CallType {
  switch (callType) {
    case 'Audio':
      return 'audio';
    case 'Video':
      return 'video';
    case 'Screen':
      return 'screen';
    default:
      return callType;
  }
}

function normalizeError(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

function buildCallId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }

  return `call-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function defaultGetUserMedia(constraints: MediaStreamConstraints): Promise<MediaStream> {
  if (
    typeof navigator === 'undefined' ||
    typeof navigator.mediaDevices?.getUserMedia !== 'function'
  ) {
    return Promise.reject(new Error('getUserMedia is unavailable'));
  }

  return navigator.mediaDevices.getUserMedia(constraints);
}

function defaultGetDisplayMedia(constraints: DisplayMediaStreamOptions): Promise<MediaStream> {
  if (
    typeof navigator === 'undefined' ||
    typeof navigator.mediaDevices?.getDisplayMedia !== 'function'
  ) {
    return Promise.reject(new Error('getDisplayMedia is unavailable'));
  }

  return navigator.mediaDevices.getDisplayMedia(constraints);
}

function defaultCreatePeerConnection(configuration: RTCConfiguration): RTCPeerConnection {
  if (typeof RTCPeerConnection !== 'function') {
    throw new Error('RTCPeerConnection is unavailable');
  }

  return new RTCPeerConnection(configuration);
}

async function defaultEnumerateDevices(): Promise<MediaDeviceInfo[]> {
  if (
    typeof navigator === 'undefined' ||
    !navigator.mediaDevices ||
    !navigator.mediaDevices.enumerateDevices
  ) {
    throw new Error('enumerateDevices is unavailable');
  }
  return navigator.mediaDevices.enumerateDevices();
}

function defaultCreateMediaStream(): MediaStream {
  if (typeof MediaStream !== 'function') {
    throw new Error('MediaStream is unavailable');
  }

  return new MediaStream();
}

function setElementStream(element: HTMLMediaElement | null, stream: MediaStream | null): void {
  if (!element) {
    return;
  }

  Reflect.set(element, 'srcObject', stream);
}

function toIceCandidateInit(candidate: SignalingIceCandidate): RTCIceCandidateInit {
  return {
    candidate: candidate.candidate,
    sdpMid: candidate.sdpMid ?? undefined,
    sdpMLineIndex: candidate.sdpMlineIndex ?? undefined,
  };
}

function toSignalingIceCandidate(
  candidate: RTCIceCandidate | RTCIceCandidateInit
): SignalingIceCandidate {
  const init =
    typeof (candidate as RTCIceCandidate).toJSON === 'function'
      ? (candidate as RTCIceCandidate).toJSON()
      : (candidate as RTCIceCandidateInit);

  return {
    candidate: init.candidate ?? '',
    sdpMid: init.sdpMid ?? undefined,
    sdpMlineIndex: init.sdpMLineIndex ?? undefined,
  };
}

function dedupePeerIds(peerIds: string[]): string[] {
  const uniquePeerIds = new Set<string>();
  const normalizedPeerIds: string[] = [];

  for (const peerId of peerIds) {
    const normalizedPeerId = peerId.trim();
    if (!normalizedPeerId || uniquePeerIds.has(normalizedPeerId)) {
      continue;
    }

    uniquePeerIds.add(normalizedPeerId);
    normalizedPeerIds.push(normalizedPeerId);
  }

  return normalizedPeerIds;
}

function isParticipantExpansionError(error: unknown): boolean {
  const message = normalizeError(error);
  return (
    message === GROUP_CALL_CAPACITY_ERROR ||
    message === 'Cannot add a participant to an active direct call'
  );
}

function createDefaultSignaling(): WebRTCSignaling {
  return {
    async sendCallJoin(groupId, callId) {
      await invoke('send_call_join', { groupId, callId });
    },
    async sendCallLeave(callId) {
      await invoke('send_call_leave', { callId });
    },
    async sendCallOffer(peerId, callId, sdp, callType) {
      await invoke('send_call_offer', { peerId, callId, sdp, callType });
    },
    async sendCallAnswer(peerId, callId, sdp) {
      await invoke('send_call_answer', { peerId, callId, sdp });
    },
    async sendIceCandidate(peerId, callId, candidate) {
      await invoke('send_ice_candidate', {
        peerId,
        callId,
        candidate: candidate.candidate,
        sdpMid: candidate.sdpMid ?? undefined,
        sdpMlineIndex: candidate.sdpMlineIndex ?? undefined,
      });
    },
    async sendCallHangup(peerId, callId) {
      await invoke('send_call_hangup', { peerId, callId });
    },
  };
}

function createDefaultDependencies(): WebRTCDependencies {
  return {
    signaling: createDefaultSignaling(),
    supportCheck: checkWebRTCSupport,
    getUserMedia: defaultGetUserMedia,
    getDisplayMedia: defaultGetDisplayMedia,
    createPeerConnection: defaultCreatePeerConnection,
    createMediaStream: defaultCreateMediaStream,
    enumerateDevices: defaultEnumerateDevices,
    createCallId: buildCallId,
  };
}

export class WebRTCService {
  private readonly dependencies: WebRTCDependencies;
  private readonly sessions = new Map<string, InternalWebRTCCallSession>();
  private readonly listeners = new Set<SessionListener>();

  constructor(dependencies: Partial<WebRTCDependencies> = {}) {
    this.dependencies = {
      ...createDefaultDependencies(),
      ...dependencies,
    };
  }

  subscribe(listener: SessionListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  async getMediaDevices(): Promise<MediaDeviceInfo[]> {
    return this.dependencies.enumerateDevices();
  }

  toggleMute(callId: string): void {
    const session = this.requireSession(callId);
    if (!session.localStream) {
      return;
    }

    for (const track of session.localStream.getAudioTracks()) {
      track.enabled = !track.enabled;
    }

    this.notify(session);
  }

  toggleCamera(callId: string): void {
    const session = this.requireSession(callId);
    const videoTracks = session.localStream?.getVideoTracks() ?? [];
    const currentVideoTrack = videoTracks[0] ?? session.cameraTrack;
    if (!currentVideoTrack) {
      return;
    }

    const nextEnabled = !currentVideoTrack.enabled;

    for (const track of videoTracks) {
      track.enabled = nextEnabled;
    }

    if (session.cameraTrack && !videoTracks.includes(session.cameraTrack)) {
      session.cameraTrack.enabled = nextEnabled;
    }

    this.notify(session);
  }

  async switchAudioDevice(callId: string, deviceId: string): Promise<void> {
    const session = this.requireSession(callId);
    if (!session.localStream) {
      return;
    }

    const oldTracks = session.localStream.getAudioTracks();
    const wasMuted = oldTracks.length > 0 && !oldTracks[0].enabled;

    const newStream = await this.dependencies.getUserMedia({
      audio: { deviceId: { exact: deviceId } },
    });
    const newTrack = newStream.getAudioTracks()[0];
    if (!newTrack) {
      return;
    }

    newTrack.enabled = !wasMuted;

    for (const oldTrack of oldTracks) {
      oldTrack.stop();
      session.localStream.removeTrack(oldTrack);
    }
    session.localStream.addTrack(newTrack);

    for (const participant of session.participants.values()) {
      await this.replaceSenderTrack(participant, 'audio', newTrack);
    }

    this.notify(session);
  }

  async switchVideoDevice(callId: string, deviceId: string): Promise<void> {
    const session = this.requireSession(callId);
    if (!session.localStream) {
      return;
    }

    const oldTracks = session.localStream.getVideoTracks();
    const previousCameraTrack = session.cameraTrack;
    const wasCameraOff = previousCameraTrack
      ? !previousCameraTrack.enabled
      : oldTracks.length > 0 && !oldTracks[0].enabled;

    const newStream = await this.dependencies.getUserMedia({
      video: { deviceId: { exact: deviceId } },
    });
    const newTrack = newStream.getVideoTracks()[0];
    if (!newTrack) {
      return;
    }

    newTrack.enabled = !wasCameraOff;
    session.cameraTrack = newTrack;

    if (session.isScreenSharing) {
      if (previousCameraTrack && previousCameraTrack !== newTrack) {
        previousCameraTrack.stop();
      }

      this.notify(session);
      return;
    }

    for (const oldTrack of oldTracks) {
      oldTrack.stop();
      session.localStream.removeTrack(oldTrack);
    }
    session.localStream.addTrack(newTrack);

    for (const participant of session.participants.values()) {
      await this.replaceSenderTrack(participant, 'video', newTrack);
    }

    this.notify(session);
  }

  async startScreenShare(callId: string): Promise<WebRTCCallSession> {
    const session = this.requireSession(callId);
    if (session.isScreenSharing) {
      return this.snapshot(session);
    }

    if (!session.localStream) {
      throw new Error('Screen sharing is only available during an active call');
    }

    const support = this.dependencies.supportCheck();
    if (!support.supported || !support.displayMedia) {
      throw new Error('Screen sharing is not supported in this environment');
    }

    const cameraTrack = session.cameraTrack ?? session.localStream.getVideoTracks()[0];
    if (!cameraTrack) {
      throw new Error('Cannot start screen sharing without an active video track');
    }

    session.cameraTrack = cameraTrack;

    const displayStream = await this.dependencies.getDisplayMedia({ video: true });
    const screenTrack = displayStream.getVideoTracks()[0];
    if (!screenTrack) {
      this.stopStream(displayStream);
      throw new Error('getDisplayMedia did not return a video track');
    }

    screenTrack.enabled = true;
    screenTrack.onended = () => {
      void this.handleScreenShareEnded(session.callId, screenTrack);
    };

    for (const track of session.localStream.getVideoTracks()) {
      session.localStream.removeTrack(track);
      if (track !== cameraTrack) {
        track.stop();
      }
    }

    session.localStream.addTrack(screenTrack);
    session.screenShareTrack = screenTrack;
    session.isScreenSharing = true;

    await this.replaceSessionVideoTrack(session, screenTrack);
    this.notify(session);
    return this.snapshot(session);
  }

  async stopScreenShare(callId: string): Promise<WebRTCCallSession> {
    const session = this.requireSession(callId);
    return this.stopScreenShareForSession(session, true);
  }

  getSession(callId: string): WebRTCCallSession | undefined {
    const session = this.sessions.get(callId);
    return session ? this.snapshot(session) : undefined;
  }

  attachLocalElement(callId: string, element: HTMLMediaElement | null): void {
    const session = this.requireSession(callId);
    session.localElement = element;
    setElementStream(element, session.localStream);
  }

  attachRemoteElement(callId: string, element: HTMLMediaElement | null, peerId?: string): void {
    const session = this.requireSession(callId);
    const participant = this.resolveParticipant(session, peerId);
    participant.remoteElement = element;
    setElementStream(element, participant.remoteStream);
  }

  async startCall(peerId: string, callType: CallType): Promise<WebRTCCallSession> {
    return this.startOutgoingCall([peerId], callType, 'direct');
  }

  async startGroupCall(
    groupId: string,
    peerIds: string[],
    callType: CallType
  ): Promise<WebRTCCallSession> {
    return this.startOutgoingCall(peerIds, callType, 'group', groupId);
  }

  async joinGroupCall(
    callId: string,
    groupId: string | undefined,
    peerIds: string[],
    callType: CallType = 'video'
  ): Promise<WebRTCCallSession> {
    this.ensureSupport();

    const normalizedPeerIds = dedupePeerIds(peerIds);
    let session = this.sessions.get(callId);
    if (!session) {
      if (normalizedPeerIds.length === 0) {
        throw new Error(`Unknown call: ${callId}`);
      }

      session = this.createSession({
        callId,
        peerId: normalizedPeerIds[0],
        callType,
        direction: 'incoming',
        mode: 'group',
        groupId,
      });
    }

    session.mode = 'group';
    session.callType = callType;
    session.groupId = groupId ?? session.groupId;

    try {
      await this.announceGroupJoin(session);
      await this.answerPendingParticipants(session);

      for (const peerId of normalizedPeerIds) {
        if (session.participants.has(peerId)) {
          continue;
        }

        await this.createOfferForParticipant(session, peerId);
      }

      await this.answerPendingParticipants(session);
      return this.snapshot(session);
    } catch (error) {
      if (isParticipantExpansionError(error)) {
        throw error;
      }

      await this.failSession(session, error);
      throw error;
    }
  }

  async handleCallJoin(
    callId: string,
    peerId: string,
    groupId: string,
    createOffer = false,
    callType: CallType = 'video'
  ): Promise<WebRTCCallSession> {
    this.ensureSupport();

    const session =
      this.sessions.get(callId) ??
      this.createSession({
        callId,
        peerId,
        callType,
        direction: 'incoming',
        mode: 'group',
        groupId,
      });

    session.mode = 'group';
    session.groupId = groupId;
    session.callType = session.callType ?? callType;

    try {
      const existingParticipant = session.participants.get(peerId);
      if (
        existingParticipant &&
        (!createOffer ||
          existingParticipant.peerConnection?.localDescription ||
          existingParticipant.peerConnection?.remoteDescription)
      ) {
        this.notify(session);
        return this.snapshot(session);
      }

      if (createOffer) {
        await this.createOfferForParticipant(session, peerId);
      } else {
        this.ensureParticipant(session, peerId);
      }

      return this.snapshot(session);
    } catch (error) {
      if (isParticipantExpansionError(error)) {
        throw error;
      }

      await this.failSession(session, error, peerId);
      throw error;
    }
  }

  async leaveGroupCall(callId: string): Promise<void> {
    const session = this.sessions.get(callId);
    if (!session) {
      return;
    }

    let signalingError: unknown;
    if (session.groupId) {
      try {
        await this.dependencies.signaling.sendCallLeave(callId);
      } catch (error) {
        signalingError = error;
      }
    }

    await this.cleanupSession(session, { sendHangup: false });

    if (signalingError) {
      throw signalingError;
    }
  }

  async handleOffer(
    callId: string,
    peerId: string,
    sdp: string,
    callType: CallType | SignalingCallType = 'video'
  ): Promise<WebRTCCallSession> {
    this.ensureSupport();

    const normalizedCallType = normalizeCallType(callType);
    const session =
      this.sessions.get(callId) ??
      this.createSession({
        callId,
        peerId,
        callType: normalizedCallType,
        direction: 'incoming',
        mode: 'direct',
      });

    session.callType = normalizedCallType;

    try {
      const participant = this.ensureParticipant(session, peerId);
      const isRenegotiation =
        participant.localTracksAttached ||
        participant.status === 'connected' ||
        participant.status === 'disconnected' ||
        session.direction === 'outgoing';
      const peerConnection = this.ensurePeerConnection(session, participant);
      await peerConnection.setRemoteDescription({ type: 'offer', sdp });
      await this.flushPendingIceCandidates(participant);

      if (isRenegotiation) {
        await this.answerParticipant(session, participant, { renegotiation: true });
        return this.snapshot(session);
      }

      participant.status = 'new';
      this.updatePrimaryPeer(session);
      this.recomputeSessionStatus(session);
      this.notify(session);
      return this.snapshot(session);
    } catch (error) {
      if (isParticipantExpansionError(error)) {
        throw error;
      }

      await this.failSession(session, error, peerId);
      throw error;
    }
  }

  async answerCall(callId: string, peerId: string): Promise<WebRTCCallSession> {
    this.ensureSupport();

    const session = this.requireSession(callId);
    const participant = this.requireParticipant(session, peerId);

    try {
      await this.answerParticipant(session, participant);
      return this.snapshot(session);
    } catch (error) {
      await this.failSession(session, error, peerId);
      throw error;
    }
  }

  async handleAnswer(callId: string, sdp: string, peerId?: string): Promise<WebRTCCallSession> {
    const session = this.requireSession(callId);
    const participant = this.resolveParticipant(session, peerId);

    try {
      const peerConnection = this.ensurePeerConnection(session, participant);
      await peerConnection.setRemoteDescription({ type: 'answer', sdp });
      await this.flushPendingIceCandidates(participant);
      this.clearNoAnswerTimeout(participant);

      if (participant.status === 'new') {
        participant.status = 'connecting';
      }

      this.recomputeSessionStatus(session);
      this.notify(session);
      this.maybeRunQueuedRenegotiation(session, participant);
      return this.snapshot(session);
    } catch (error) {
      await this.failSession(session, error, participant.peerId);
      throw error;
    }
  }

  async handleIceCandidate(
    callId: string,
    candidate: SignalingIceCandidate,
    peerId?: string
  ): Promise<WebRTCCallSession> {
    const session = this.requireSession(callId);
    const participant = this.resolveParticipant(session, peerId);
    const peerConnection = this.ensurePeerConnection(session, participant);
    const nextCandidate = toIceCandidateInit(candidate);

    try {
      if (!peerConnection.remoteDescription) {
        participant.pendingRemoteCandidates.push(nextCandidate);
        this.notify(session);
        return this.snapshot(session);
      }

      await peerConnection.addIceCandidate(nextCandidate);
      this.notify(session);
      return this.snapshot(session);
    } catch (error) {
      await this.failSession(session, error, participant.peerId);
      throw error;
    }
  }

  async hangup(callId: string): Promise<void> {
    const session = this.sessions.get(callId);
    if (!session) {
      return;
    }

    await this.cleanupSession(session, { sendHangup: true });
  }

  async handleRemoteHangup(callId: string, peerId?: string): Promise<void> {
    const session = this.sessions.get(callId);
    if (!session) {
      return;
    }

    if (peerId) {
      const participant = session.participants.get(peerId);
      if (!participant) {
        return;
      }

      await this.cleanupParticipant(session, participant, { sendHangup: false });
      return;
    }

    await this.cleanupSession(session, { sendHangup: false });
  }

  private ensureSupport(): void {
    const support = this.dependencies.supportCheck();
    if (!support.supported) {
      throw new Error('WebRTC is not supported in this environment');
    }
  }

  private async startOutgoingCall(
    peerIds: string[],
    callType: CallType,
    mode: CallMode,
    groupId?: string
  ): Promise<WebRTCCallSession> {
    this.ensureSupport();

    const normalizedPeerIds = dedupePeerIds(peerIds);
    if (normalizedPeerIds.length === 0) {
      throw new Error('At least one peer is required to start a call');
    }

    if (mode === 'direct' && normalizedPeerIds.length !== 1) {
      throw new Error('Direct calls require exactly one peer');
    }

    if (mode === 'group' && normalizedPeerIds.length > MAX_GROUP_REMOTE_PARTICIPANTS) {
      throw new Error(GROUP_CALL_CAPACITY_ERROR);
    }

    const session = this.createSession({
      callId: this.dependencies.createCallId(),
      peerId: normalizedPeerIds[0],
      callType,
      direction: 'outgoing',
      mode,
      groupId,
    });

    try {
      await this.ensureLocalStream(session);
      await this.announceGroupJoin(session);
      for (const peerId of normalizedPeerIds) {
        await this.createOfferForParticipant(session, peerId);
      }

      return this.snapshot(session);
    } catch (error) {
      await this.failSession(session, error);
      throw error;
    }
  }

  private async announceGroupJoin(session: InternalWebRTCCallSession): Promise<void> {
    if (session.mode !== 'group' || !session.groupId) {
      return;
    }

    await this.dependencies.signaling.sendCallJoin(session.groupId, session.callId);
  }

  private createSession(input: {
    callId: string;
    peerId: string;
    callType: CallType;
    direction: CallDirection;
    mode: CallMode;
    groupId?: string;
  }): InternalWebRTCCallSession {
    const session: InternalWebRTCCallSession = {
      callId: input.callId,
      groupId: input.groupId,
      peerId: input.peerId,
      callType: input.callType,
      direction: input.direction,
      status: 'new',
      localStream: null,
      lastError: undefined,
      mode: input.mode,
      participants: new Map(),
      localElement: null,
      cleanupInProgress: false,
      cameraTrack: null,
      screenShareTrack: null,
      isScreenSharing: false,
    };

    this.sessions.set(session.callId, session);
    this.notify(session);
    return session;
  }

  private requireSession(callId: string): InternalWebRTCCallSession {
    const session = this.sessions.get(callId);
    if (!session) {
      throw new Error(`Unknown call: ${callId}`);
    }

    return session;
  }

  private requireParticipant(
    session: InternalWebRTCCallSession,
    peerId: string
  ): InternalParticipantSession {
    const participant = session.participants.get(peerId);
    if (!participant) {
      throw new Error(`Unknown call participant: ${peerId}`);
    }

    return participant;
  }

  private resolveParticipant(
    session: InternalWebRTCCallSession,
    peerId?: string
  ): InternalParticipantSession {
    if (peerId) {
      return this.requireParticipant(session, peerId);
    }

    if (session.peerId && session.participants.has(session.peerId)) {
      return this.requireParticipant(session, session.peerId);
    }

    if (session.participants.size === 1) {
      return Array.from(session.participants.values())[0];
    }

    throw new Error(`Peer ID is required for call ${session.callId}`);
  }

  private ensureParticipant(
    session: InternalWebRTCCallSession,
    peerId: string
  ): InternalParticipantSession {
    const existing = session.participants.get(peerId);
    if (existing) {
      return existing;
    }

    this.ensureParticipantCapacity(session);

    const participant: InternalParticipantSession = {
      peerId,
      status: 'new',
      remoteStream: null,
      lastError: undefined,
      peerConnection: null,
      pendingRemoteCandidates: [],
      localTracksAttached: false,
      remoteElement: null,
      cleanupInProgress: false,
      noAnswerTimeout: null,
      disconnectTimeout: null,
      renegotiationInProgress: false,
      renegotiationQueued: false,
    };

    session.participants.set(peerId, participant);
    if (session.participants.size > 1) {
      session.mode = 'group';
    }

    this.updatePrimaryPeer(session);
    this.recomputeSessionStatus(session);
    this.notify(session);
    return participant;
  }

  private ensureParticipantCapacity(session: InternalWebRTCCallSession): void {
    if (session.mode === 'direct' && session.participants.size > 0) {
      if (session.direction === 'incoming') {
        session.mode = 'group';
      } else {
        throw new Error('Cannot add a participant to an active direct call');
      }
    }

    if (session.mode === 'group' && session.participants.size >= MAX_GROUP_REMOTE_PARTICIPANTS) {
      throw new Error(GROUP_CALL_CAPACITY_ERROR);
    }
  }

  private ensurePeerConnection(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): RTCPeerConnection {
    if (participant.peerConnection) {
      return participant.peerConnection;
    }

    const peerConnection = this.dependencies.createPeerConnection(RTC_CONFIGURATION);
    participant.peerConnection = peerConnection;

    peerConnection.onicecandidate = (event) => {
      if (!event.candidate) {
        return;
      }

      void this.sendLocalIceCandidate(session, participant, event.candidate);
    };

    peerConnection.ontrack = (event) => {
      if (event.streams[0]) {
        participant.remoteStream = event.streams[0];
      } else {
        const remoteStream = participant.remoteStream ?? this.dependencies.createMediaStream();
        const existingTracks = remoteStream.getTracks();
        if (!existingTracks.includes(event.track)) {
          remoteStream.addTrack(event.track);
        }
        participant.remoteStream = remoteStream;
      }

      setElementStream(participant.remoteElement, participant.remoteStream);
      this.notify(session);
    };

    peerConnection.onconnectionstatechange = () => {
      void this.handlePeerConnectionStateChange(session, participant);
    };

    peerConnection.oniceconnectionstatechange = () => {
      void this.handleIceConnectionStateChange(session, participant);
    };

    peerConnection.onnegotiationneeded = () => {
      this.queueRenegotiation(session, participant);
    };

    this.notify(session);
    return peerConnection;
  }

  private async ensureLocalStream(session: InternalWebRTCCallSession): Promise<MediaStream> {
    if (!session.localStream) {
      session.localStream = await this.dependencies.getUserMedia({
        audio: true,
        video: session.callType !== 'audio',
      });
      session.cameraTrack = session.localStream.getVideoTracks()[0] ?? null;
      setElementStream(session.localElement, session.localStream);
      this.notify(session);
    }

    return session.localStream;
  }

  private async handleScreenShareEnded(
    callId: string,
    screenTrack: MediaStreamTrack
  ): Promise<void> {
    const session = this.sessions.get(callId);
    if (!session || !session.isScreenSharing || session.screenShareTrack !== screenTrack) {
      return;
    }

    try {
      await this.stopScreenShareForSession(session, false);
    } catch (error) {
      await this.failSession(session, error);
    }
  }

  private attachLocalTracks(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession,
    peerConnection: RTCPeerConnection
  ): void {
    if (!session.localStream || participant.localTracksAttached) {
      return;
    }

    for (const track of session.localStream.getTracks()) {
      peerConnection.addTrack(track, session.localStream);
    }
    participant.localTracksAttached = true;
  }

  private async createOfferForParticipant(
    session: InternalWebRTCCallSession,
    peerId: string,
    options: { renegotiation?: boolean } = {}
  ): Promise<void> {
    const participant = this.ensureParticipant(session, peerId);
    const peerConnection = this.ensurePeerConnection(session, participant);
    await this.ensureLocalStream(session);
    this.attachLocalTracks(session, participant, peerConnection);

    const offer = options.renegotiation
      ? await peerConnection.createOffer({ iceRestart: true })
      : await peerConnection.createOffer();
    await peerConnection.setLocalDescription(offer);

    if (!offer.sdp) {
      throw new Error('RTCPeerConnection returned an offer without SDP');
    }

    if (!options.renegotiation) {
      participant.status = 'connecting';
      this.recomputeSessionStatus(session);
      this.notify(session);
    }

    await this.dependencies.signaling.sendCallOffer(
      peerId,
      session.callId,
      offer.sdp,
      SIGNALING_CALL_TYPES[session.callType]
    );

    if (!options.renegotiation) {
      this.startNoAnswerTimeout(session, participant);
    }
  }

  private async answerPendingParticipants(session: InternalWebRTCCallSession): Promise<void> {
    for (const participant of session.participants.values()) {
      if (!this.canAnswerParticipant(participant)) {
        continue;
      }

      await this.answerParticipant(session, participant);
    }
  }

  private canAnswerParticipant(participant: InternalParticipantSession): boolean {
    return (
      !!participant.peerConnection?.remoteDescription &&
      participant.peerConnection.remoteDescription.type === 'offer' &&
      !participant.peerConnection.localDescription
    );
  }

  private async answerParticipant(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession,
    options: { renegotiation?: boolean } = {}
  ): Promise<void> {
    const peerConnection = this.ensurePeerConnection(session, participant);
    if (!peerConnection.remoteDescription) {
      throw new Error('Cannot answer a call before handling its offer');
    }

    await this.ensureLocalStream(session);
    this.attachLocalTracks(session, participant, peerConnection);

    const answer = await peerConnection.createAnswer();
    await peerConnection.setLocalDescription(answer);

    if (!answer.sdp) {
      throw new Error('RTCPeerConnection returned an answer without SDP');
    }

    this.clearNoAnswerTimeout(participant);

    if (!options.renegotiation) {
      participant.status = 'connecting';
      this.recomputeSessionStatus(session);
      this.notify(session);
    }

    await this.dependencies.signaling.sendCallAnswer(
      participant.peerId,
      session.callId,
      answer.sdp
    );

    this.maybeRunQueuedRenegotiation(session, participant);
  }

  private async replaceSenderTrack(
    participant: InternalParticipantSession,
    kind: MediaStreamTrack['kind'],
    nextTrack: MediaStreamTrack
  ): Promise<void> {
    if (!participant.peerConnection) {
      return;
    }

    const sender = participant.peerConnection
      .getSenders()
      .find((candidate) => candidate.track?.kind === kind);

    if (sender) {
      await sender.replaceTrack(nextTrack);
    }
  }

  private async replaceSessionVideoTrack(
    session: InternalWebRTCCallSession,
    nextTrack: MediaStreamTrack
  ): Promise<void> {
    for (const participant of session.participants.values()) {
      await this.replaceSenderTrack(participant, 'video', nextTrack);
    }
  }

  private async stopScreenShareForSession(
    session: InternalWebRTCCallSession,
    stopTrack: boolean
  ): Promise<WebRTCCallSession> {
    if (!session.isScreenSharing || !session.localStream) {
      return this.snapshot(session);
    }

    const cameraTrack = session.cameraTrack;
    if (!cameraTrack) {
      throw new Error('Cannot restore camera after screen sharing');
    }

    const screenTrack = session.screenShareTrack ?? session.localStream.getVideoTracks()[0] ?? null;
    session.screenShareTrack = null;
    session.isScreenSharing = false;

    if (screenTrack) {
      screenTrack.onended = null;
      session.localStream.removeTrack(screenTrack);
      if (stopTrack) {
        screenTrack.stop();
      }
    }

    if (!session.localStream.getVideoTracks().includes(cameraTrack)) {
      session.localStream.addTrack(cameraTrack);
    }

    await this.replaceSessionVideoTrack(session, cameraTrack);
    this.notify(session);
    return this.snapshot(session);
  }

  private clearNoAnswerTimeout(participant: InternalParticipantSession): void {
    if (participant.noAnswerTimeout) {
      clearTimeout(participant.noAnswerTimeout);
      participant.noAnswerTimeout = null;
    }
  }

  private startNoAnswerTimeout(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): void {
    this.clearNoAnswerTimeout(participant);
    participant.noAnswerTimeout = setTimeout(() => {
      void this.handleNoAnswerTimeout(session.callId, participant.peerId);
    }, CALL_NO_ANSWER_TIMEOUT_MS);
  }

  private async handleNoAnswerTimeout(callId: string, peerId: string): Promise<void> {
    const session = this.sessions.get(callId);
    const participant = session?.participants.get(peerId);
    if (!session || !participant || session.cleanupInProgress || participant.cleanupInProgress) {
      return;
    }

    participant.noAnswerTimeout = null;
    if (participant.status === 'connected' || participant.status === 'closed') {
      return;
    }

    participant.lastError = NO_ANSWER_ERROR;

    if (session.participants.size === 1) {
      session.lastError = NO_ANSWER_ERROR;
      await this.cleanupSession(session, { sendHangup: true });
      return;
    }

    await this.cleanupParticipant(session, participant, { sendHangup: true });
  }

  private clearDisconnectTimeout(participant: InternalParticipantSession): void {
    if (participant.disconnectTimeout) {
      clearTimeout(participant.disconnectTimeout);
      participant.disconnectTimeout = null;
    }
  }

  private startDisconnectTimeout(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): void {
    this.clearDisconnectTimeout(participant);
    participant.disconnectTimeout = setTimeout(() => {
      void this.handleDisconnectTimeout(session.callId, participant.peerId);
    }, CALL_DISCONNECT_TIMEOUT_MS);
  }

  private async handleDisconnectTimeout(callId: string, peerId: string): Promise<void> {
    const session = this.sessions.get(callId);
    const participant = session?.participants.get(peerId);
    if (!session || !participant || session.cleanupInProgress || participant.cleanupInProgress) {
      return;
    }

    participant.disconnectTimeout = null;

    const iceState = participant.peerConnection?.iceConnectionState;
    if (iceState !== 'disconnected') {
      return;
    }

    participant.lastError = CALL_DISCONNECTED_ERROR;

    if (session.participants.size === 1) {
      session.lastError = CALL_DISCONNECTED_ERROR;
      await this.cleanupSession(session, { sendHangup: true });
      return;
    }

    await this.cleanupParticipant(session, participant, { sendHangup: true });
  }

  private queueRenegotiation(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): void {
    if (
      session.cleanupInProgress ||
      participant.cleanupInProgress ||
      (participant.status !== 'connected' && participant.status !== 'disconnected')
    ) {
      return;
    }

    if (
      participant.renegotiationInProgress ||
      participant.peerConnection?.signalingState !== 'stable'
    ) {
      participant.renegotiationQueued = true;
      return;
    }

    participant.renegotiationQueued = false;
    participant.renegotiationInProgress = true;

    void this.renegotiateParticipant(session.callId, participant.peerId);
  }

  private async renegotiateParticipant(callId: string, peerId: string): Promise<void> {
    const session = this.sessions.get(callId);
    const participant = session?.participants.get(peerId);
    if (!session || !participant || session.cleanupInProgress || participant.cleanupInProgress) {
      return;
    }

    try {
      if (
        !participant.peerConnection ||
        !session.localStream ||
        !participant.localTracksAttached ||
        participant.peerConnection.signalingState !== 'stable'
      ) {
        participant.renegotiationQueued = true;
        return;
      }

      await this.createOfferForParticipant(session, peerId, { renegotiation: true });
    } catch (error) {
      await this.failSession(session, error, peerId);
    } finally {
      participant.renegotiationInProgress = false;
      this.maybeRunQueuedRenegotiation(session, participant);
    }
  }

  private maybeRunQueuedRenegotiation(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): void {
    if (
      !participant.renegotiationQueued ||
      participant.renegotiationInProgress ||
      !participant.peerConnection ||
      participant.peerConnection.signalingState !== 'stable'
    ) {
      return;
    }

    this.queueRenegotiation(session, participant);
  }

  private async sendLocalIceCandidate(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession,
    candidate: RTCIceCandidate
  ): Promise<void> {
    try {
      await this.dependencies.signaling.sendIceCandidate(
        participant.peerId,
        session.callId,
        toSignalingIceCandidate(candidate)
      );
    } catch (error) {
      await this.failSession(session, error, participant.peerId);
    }
  }

  private async flushPendingIceCandidates(participant: InternalParticipantSession): Promise<void> {
    if (!participant.peerConnection || !participant.peerConnection.remoteDescription) {
      return;
    }

    while (participant.pendingRemoteCandidates.length > 0) {
      const candidate = participant.pendingRemoteCandidates.shift();
      if (!candidate) {
        continue;
      }

      await participant.peerConnection.addIceCandidate(candidate);
    }
  }

  private async handlePeerConnectionStateChange(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): Promise<void> {
    if (session.cleanupInProgress || participant.cleanupInProgress || session.status === 'closed') {
      return;
    }

    const state = participant.peerConnection?.connectionState;
    if (!state) {
      return;
    }

    if (state === 'connected') {
      this.clearNoAnswerTimeout(participant);
      this.clearDisconnectTimeout(participant);
      participant.lastError = undefined;
      participant.status = 'connected';
      this.recomputeSessionStatus(session);
      this.notify(session);
      return;
    }

    if (state === 'failed') {
      this.clearNoAnswerTimeout(participant);
      this.clearDisconnectTimeout(participant);
      await this.failSession(session, new Error(CALL_DISCONNECTED_ERROR), participant.peerId);
      return;
    }

    if (state === 'closed') {
      await this.cleanupParticipant(session, participant, { sendHangup: false });
    }
  }

  private async handleIceConnectionStateChange(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession
  ): Promise<void> {
    if (session.cleanupInProgress || participant.cleanupInProgress || session.status === 'closed') {
      return;
    }

    const state = participant.peerConnection?.iceConnectionState;
    if (!state) {
      return;
    }

    if (state === 'connected' || state === 'completed') {
      this.clearDisconnectTimeout(participant);
      if (participant.status === 'disconnected') {
        participant.lastError = undefined;
        participant.status = 'connected';
        this.recomputeSessionStatus(session);
        this.notify(session);
      }
      return;
    }

    if (state === 'disconnected') {
      this.startDisconnectTimeout(session, participant);
      participant.status = 'disconnected';
      this.recomputeSessionStatus(session);
      this.notify(session);
      return;
    }

    if (state === 'failed') {
      this.clearDisconnectTimeout(participant);
      this.clearNoAnswerTimeout(participant);
      await this.failSession(session, new Error(CALL_DISCONNECTED_ERROR), participant.peerId);
    }
  }

  private async failSession(
    session: InternalWebRTCCallSession,
    error: unknown,
    peerId?: string
  ): Promise<void> {
    const errorMessage = normalizeError(error);

    if (peerId) {
      const participant = session.participants.get(peerId);
      if (participant) {
        participant.lastError = errorMessage;
      }
      session.lastError = errorMessage;

      if (session.participants.size > 1) {
        if (participant) {
          await this.cleanupParticipant(session, participant, { sendHangup: false });
        }
        return;
      }
    }

    session.lastError = errorMessage;
    await this.cleanupSession(session, { sendHangup: false });
  }

  private async cleanupSession(
    session: InternalWebRTCCallSession,
    options: { sendHangup: boolean }
  ): Promise<void> {
    if (session.cleanupInProgress) {
      return;
    }

    session.cleanupInProgress = true;

    let signalingError: unknown;
    const participants = Array.from(session.participants.values());

    if (options.sendHangup) {
      for (const participant of participants) {
        try {
          await this.dependencies.signaling.sendCallHangup(participant.peerId, session.callId);
        } catch (error) {
          signalingError ??= error;
          participant.lastError = normalizeError(error);
        }
      }
    }

    for (const participant of participants) {
      participant.cleanupInProgress = true;
      this.cleanupParticipantResources(participant);
      participant.cleanupInProgress = false;
    }

    session.participants.clear();
    this.cleanupLocalMedia(session);
    session.peerId = '';
    session.status = 'closed';
    setElementStream(session.localElement, null);

    if (signalingError) {
      session.lastError = normalizeError(signalingError);
    }

    this.notify(session);
    session.cleanupInProgress = false;

    if (signalingError) {
      throw signalingError;
    }
  }

  private async cleanupParticipant(
    session: InternalWebRTCCallSession,
    participant: InternalParticipantSession,
    options: { sendHangup: boolean }
  ): Promise<void> {
    if (session.cleanupInProgress || participant.cleanupInProgress) {
      return;
    }

    participant.cleanupInProgress = true;

    let signalingError: unknown;
    if (options.sendHangup) {
      try {
        await this.dependencies.signaling.sendCallHangup(participant.peerId, session.callId);
      } catch (error) {
        signalingError = error;
        participant.lastError = normalizeError(error);
        session.lastError = participant.lastError;
      }
    }

    this.cleanupParticipantResources(participant);
    session.participants.delete(participant.peerId);
    this.updatePrimaryPeer(session);

    if (session.participants.size === 0) {
      this.cleanupLocalMedia(session);
      session.status = 'closed';
      setElementStream(session.localElement, null);
    } else {
      this.recomputeSessionStatus(session);
    }

    this.notify(session);
    participant.cleanupInProgress = false;

    if (signalingError) {
      throw signalingError;
    }
  }

  private cleanupParticipantResources(participant: InternalParticipantSession): void {
    this.clearNoAnswerTimeout(participant);
    this.clearDisconnectTimeout(participant);
    participant.renegotiationInProgress = false;
    participant.renegotiationQueued = false;
    this.stopStream(participant.remoteStream);
    participant.remoteStream = null;
    participant.pendingRemoteCandidates = [];
    participant.localTracksAttached = false;
    participant.status = 'closed';
    setElementStream(participant.remoteElement, null);

    if (participant.peerConnection && participant.peerConnection.connectionState !== 'closed') {
      participant.peerConnection.onnegotiationneeded = null;
      participant.peerConnection.close();
    }

    participant.peerConnection = null;
  }

  private cleanupLocalMedia(session: InternalWebRTCCallSession): void {
    const stoppedTracks = new Set<MediaStreamTrack>();

    if (session.screenShareTrack) {
      session.screenShareTrack.onended = null;
    }

    for (const track of session.localStream?.getTracks() ?? []) {
      stoppedTracks.add(track);
      track.stop();
    }

    if (session.cameraTrack && !stoppedTracks.has(session.cameraTrack)) {
      session.cameraTrack.stop();
    }

    if (session.screenShareTrack && !stoppedTracks.has(session.screenShareTrack)) {
      session.screenShareTrack.stop();
    }

    session.localStream = null;
    session.cameraTrack = null;
    session.screenShareTrack = null;
    session.isScreenSharing = false;
  }

  private updatePrimaryPeer(session: InternalWebRTCCallSession): void {
    const currentPrimaryPeer = session.peerId
      ? session.participants.get(session.peerId)
      : undefined;
    if (currentPrimaryPeer) {
      return;
    }

    const nextPrimaryPeer = session.participants.keys().next();
    session.peerId = nextPrimaryPeer.done ? '' : nextPrimaryPeer.value;
  }

  private recomputeSessionStatus(session: InternalWebRTCCallSession): void {
    const participantStatuses = Array.from(session.participants.values())
      .map((participant) => participant.status)
      .filter((status) => status !== 'closed');

    if (participantStatuses.length === 0) {
      session.status = 'closed';
      return;
    }

    if (participantStatuses.includes('connected')) {
      session.status = 'connected';
      return;
    }

    if (participantStatuses.includes('connecting')) {
      session.status = 'connecting';
      return;
    }

    if (participantStatuses.includes('new')) {
      session.status = 'new';
      return;
    }

    session.status = 'disconnected';
  }

  private stopStream(stream: MediaStream | null): void {
    if (!stream) {
      return;
    }

    for (const track of stream.getTracks()) {
      track.stop();
    }
  }

  private notify(session: InternalWebRTCCallSession): void {
    const snapshot = this.snapshot(session);
    for (const listener of this.listeners) {
      listener(snapshot);
    }
  }

  private snapshot(session: InternalWebRTCCallSession): WebRTCCallSession {
    const participants = Array.from(session.participants.values()).map((participant) => ({
      peerId: participant.peerId,
      status: participant.status,
      remoteStream: participant.remoteStream,
      lastError: participant.lastError,
    }));
    const primaryParticipant =
      participants.find((participant) => participant.peerId === session.peerId) ?? participants[0];

    return {
      callId: session.callId,
      groupId: session.groupId,
      peerId: session.peerId,
      callType: session.callType,
      direction: session.direction,
      status: session.status,
      localStream: session.localStream,
      remoteStream: primaryParticipant?.remoteStream ?? null,
      isScreenSharing: session.isScreenSharing,
      lastError: session.lastError,
      mode: session.mode,
      participants,
    };
  }
}
