import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import {
  CALL_DISCONNECT_TIMEOUT_MS,
  CALL_NO_ANSWER_TIMEOUT_MS,
  GROUP_CALL_CAPACITY_ERROR,
  WebRTCService,
} from './webrtc';

class MockMediaStreamTrack {
  enabled = true;
  readonly id: string;
  readonly label: string;
  readonly muted = false;
  readonly readyState: MediaStreamTrackState = 'live';
  onended: ((event: Event) => void) | null = null;
  readonly stop = vi.fn();

  constructor(
    readonly kind: 'audio' | 'video',
    id: string
  ) {
    this.id = id;
    this.label = `${kind}-${id}`;
  }

  emitEnded(): void {
    this.onended?.(new Event('ended'));
  }
}

class MockMediaStream {
  private readonly tracks: MediaStreamTrack[];

  constructor(tracks: MediaStreamTrack[] = []) {
    this.tracks = [...tracks];
  }

  addTrack(track: MediaStreamTrack): void {
    this.tracks.push(track);
  }

  removeTrack(track: MediaStreamTrack): void {
    const index = this.tracks.indexOf(track);
    if (index !== -1) {
      this.tracks.splice(index, 1);
    }
  }

  getTracks(): MediaStreamTrack[] {
    return [...this.tracks];
  }

  getAudioTracks(): MediaStreamTrack[] {
    return this.tracks.filter((track) => track.kind === 'audio');
  }

  getVideoTracks(): MediaStreamTrack[] {
    return this.tracks.filter((track) => track.kind === 'video');
  }
}

class MockRTCPeerConnection {
  static instances: MockRTCPeerConnection[] = [];

  senders: any[] = [];
  private answerCount = 0;
  private offerCount = 0;
  getSenders = vi.fn(() => this.senders);

  readonly addIceCandidate = vi.fn(async (candidate: RTCIceCandidateInit) => {
    this.remoteCandidates.push(candidate);
  });
  readonly addTrack = vi.fn((track: MediaStreamTrack, stream: MediaStream) => {
    this.addedTracks.push({ track, stream });
    const sender = {
      track,
      replaceTrack: vi.fn(async (newTrack) => {
        sender.track = newTrack;
      }),
    };
    this.senders.push(sender);
    return sender as any;
  });
  readonly close = vi.fn(() => {
    if (this.connectionState === 'closed') {
      return;
    }

    this.connectionState = 'closed';
    this.iceConnectionState = 'closed';
    this.signalingState = 'closed';
    this.onconnectionstatechange?.(new Event('connectionstatechange'));
  });
  readonly createAnswer = vi.fn(async () => {
    this.answerCount += 1;
    return {
      type: 'answer' as const,
      sdp:
        this.answerCount === 1
          ? `answer-sdp-${MockRTCPeerConnection.instances.length}`
          : `answer-sdp-${MockRTCPeerConnection.instances.length}-${this.answerCount}`,
    };
  });
  readonly createOffer = vi.fn(async (_options?: RTCOfferOptions) => {
    this.offerCount += 1;
    return {
      type: 'offer' as const,
      sdp:
        this.offerCount === 1
          ? `offer-sdp-${MockRTCPeerConnection.instances.length}`
          : `offer-sdp-${MockRTCPeerConnection.instances.length}-${this.offerCount}`,
    };
  });
  readonly setLocalDescription = vi.fn(async (description: RTCSessionDescriptionInit) => {
    this.localDescription = description;
    this.signalingState = description.type === 'offer' ? 'have-local-offer' : 'stable';
  });
  readonly setRemoteDescription = vi.fn(async (description: RTCSessionDescriptionInit) => {
    this.remoteDescription = description;
    this.signalingState = description.type === 'offer' ? 'have-remote-offer' : 'stable';
  });

  connectionState: RTCPeerConnectionState = 'new';
  iceConnectionState: RTCIceConnectionState = 'new';
  localDescription: RTCSessionDescriptionInit | null = null;
  onconnectionstatechange: ((event: Event) => void) | null = null;
  onicecandidate: ((event: RTCPeerConnectionIceEvent) => void) | null = null;
  oniceconnectionstatechange: ((event: Event) => void) | null = null;
  onnegotiationneeded: ((event: Event) => void) | null = null;
  ontrack: ((event: RTCTrackEvent) => void) | null = null;
  remoteDescription: RTCSessionDescriptionInit | null = null;
  signalingState: RTCSignalingState = 'stable';

  readonly addedTracks: Array<{ track: MediaStreamTrack; stream: MediaStream }> = [];
  readonly remoteCandidates: RTCIceCandidateInit[] = [];

  constructor(readonly configuration: RTCConfiguration) {
    MockRTCPeerConnection.instances.push(this);
  }

  emitIceCandidate(candidate: RTCIceCandidateInit | null): void {
    this.onicecandidate?.({
      candidate: candidate
        ? {
            ...candidate,
            toJSON: () => candidate,
          }
        : null,
    } as RTCPeerConnectionIceEvent);
  }

  emitTrack(track: MediaStreamTrack, streams: MediaStream[] = []): void {
    this.ontrack?.({ track, streams } as unknown as RTCTrackEvent);
  }

  emitNegotiationNeeded(): void {
    this.onnegotiationneeded?.(new Event('negotiationneeded'));
  }

  setConnectionState(state: RTCPeerConnectionState): void {
    this.connectionState = state;
    this.onconnectionstatechange?.(new Event('connectionstatechange'));
  }

  setIceConnectionState(state: RTCIceConnectionState): void {
    this.iceConnectionState = state;
    this.oniceconnectionstatechange?.(new Event('iceconnectionstatechange'));
  }
}

function createTrack(kind: 'audio' | 'video', id: string): MediaStreamTrack {
  return new MockMediaStreamTrack(kind, id) as unknown as MediaStreamTrack;
}

function createStream(tracks: MediaStreamTrack[] = []): MediaStream {
  return new MockMediaStream(tracks) as unknown as MediaStream;
}

function streamTracks(stream: MediaStream): MockMediaStreamTrack[] {
  return stream.getTracks() as unknown as MockMediaStreamTrack[];
}

async function flushTasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

function createService(options: { callId?: string } = {}) {
  const enumerateDevicesMock = vi.fn(async () => [
    { deviceId: 'default-audio', kind: 'audioinput', label: 'Default Audio' },
    { deviceId: 'default-video', kind: 'videoinput', label: 'Default Video' },
  ]);

  const getUserMediaMock = vi.fn(async (constraints: MediaStreamConstraints) => {
    const tracks = [createTrack('audio', `${options.callId ?? 'call'}-audio`)];
    if (constraints.video) {
      tracks.push(createTrack('video', `${options.callId ?? 'call'}-video`));
    }

    return createStream(tracks);
  });

  const getDisplayMediaMock = vi.fn(async () =>
    createStream([createTrack('video', `${options.callId ?? 'call'}-screen`)])
  );

  const service = new WebRTCService({
    createCallId: () => options.callId ?? 'call-1',
    createMediaStream: () => createStream(),
    enumerateDevices: enumerateDevicesMock as any,
    createPeerConnection: (configuration) =>
      new MockRTCPeerConnection(configuration) as unknown as RTCPeerConnection,
    getDisplayMedia: getDisplayMediaMock,
    getUserMedia: getUserMediaMock,
    supportCheck: () => ({
      supported: true,
      getUserMedia: true,
      peerConnection: true,
      displayMedia: true,
    }),
  });

  return { getUserMediaMock, getDisplayMediaMock, enumerateDevicesMock, service };
}

describe('WebRTCService', () => {
  beforeEach(() => {
    vi.useRealTimers();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    MockRTCPeerConnection.instances = [];
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('starts an outgoing call, handles the answer, and reaches connected state', async () => {
    const { getUserMediaMock, service } = createService();
    const localVideo = document.createElement('video');
    const remoteVideo = document.createElement('video');

    const session = await service.startCall('peer-1', 'video');
    service.attachLocalElement(session.callId, localVideo);
    service.attachRemoteElement(session.callId, remoteVideo);

    const peerConnection = MockRTCPeerConnection.instances[0];
    expect(peerConnection.configuration).toEqual({ iceServers: [] });
    expect(getUserMediaMock).toHaveBeenCalledWith({ audio: true, video: true });
    expect((localVideo as HTMLVideoElement & { srcObject?: MediaStream | null }).srcObject).toBe(
      session.localStream
    );
    expect(invokeMock).toHaveBeenCalledWith('send_call_offer', {
      peerId: 'peer-1',
      callId: 'call-1',
      sdp: 'offer-sdp-1',
      callType: 'Video',
    });

    await service.handleAnswer(session.callId, 'answer-sdp-remote');

    const remoteTrack = createTrack('video', 'remote-video');
    const remoteStream = createStream([remoteTrack]);
    peerConnection.emitTrack(remoteTrack, [remoteStream]);
    peerConnection.setConnectionState('connected');
    await flushTasks();

    expect(peerConnection.remoteDescription).toEqual({ type: 'answer', sdp: 'answer-sdp-remote' });
    expect((remoteVideo as HTMLVideoElement & { srcObject?: MediaStream | null }).srcObject).toBe(
      remoteStream
    );
    expect(service.getSession(session.callId)).toMatchObject({
      callId: 'call-1',
      peerId: 'peer-1',
      callType: 'video',
      status: 'connected',
      remoteStream,
    });
  });

  it('handles an incoming offer and answers audio calls without requesting video', async () => {
    const { getUserMediaMock, service } = createService({ callId: 'incoming-1' });

    await service.handleOffer('incoming-1', 'peer-2', 'offer-sdp-remote', 'Audio');
    const session = await service.answerCall('incoming-1', 'peer-2');

    const peerConnection = MockRTCPeerConnection.instances[0];
    expect(peerConnection.remoteDescription).toEqual({ type: 'offer', sdp: 'offer-sdp-remote' });
    expect(peerConnection.addedTracks).toHaveLength(1);
    expect(getUserMediaMock).toHaveBeenCalledWith({ audio: true, video: false });
    expect(invokeMock).toHaveBeenCalledWith('send_call_answer', {
      peerId: 'peer-2',
      callId: 'incoming-1',
      sdp: 'answer-sdp-1',
    });
    expect(session).toMatchObject({
      callId: 'incoming-1',
      peerId: 'peer-2',
      callType: 'audio',
      direction: 'incoming',
      status: 'connecting',
    });
  });

  it('queues remote ICE until the answer arrives and sends local ICE candidates through signaling', async () => {
    const { service } = createService({ callId: 'ice-call' });
    const session = await service.startCall('peer-3', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];

    await service.handleIceCandidate('ice-call', {
      candidate: 'candidate:remote',
      sdpMid: '0',
      sdpMlineIndex: 0,
    });

    expect(peerConnection.addIceCandidate).not.toHaveBeenCalled();

    peerConnection.emitIceCandidate({
      candidate: 'candidate:local',
      sdpMid: '0',
      sdpMLineIndex: 1,
    });
    await flushTasks();

    expect(invokeMock).toHaveBeenNthCalledWith(2, 'send_ice_candidate', {
      peerId: 'peer-3',
      callId: 'ice-call',
      candidate: 'candidate:local',
      sdpMid: '0',
      sdpMlineIndex: 1,
    });

    await service.handleAnswer(session.callId, 'answer-sdp-ice');

    expect(peerConnection.addIceCandidate).toHaveBeenCalledWith({
      candidate: 'candidate:remote',
      sdpMid: '0',
      sdpMLineIndex: 0,
    });
  });

  it('starts a group call and creates offers for each group participant', async () => {
    const { getUserMediaMock, service } = createService({ callId: 'group-call' });

    const session = await service.startGroupCall('group-1', ['peer-6', 'peer-7'], 'video');

    expect(MockRTCPeerConnection.instances).toHaveLength(2);
    expect(getUserMediaMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenNthCalledWith(1, 'send_call_join', {
      groupId: 'group-1',
      callId: 'group-call',
    });
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'send_call_offer', {
      peerId: 'peer-6',
      callId: 'group-call',
      sdp: 'offer-sdp-1',
      callType: 'Video',
    });
    expect(invokeMock).toHaveBeenNthCalledWith(3, 'send_call_offer', {
      peerId: 'peer-7',
      callId: 'group-call',
      sdp: 'offer-sdp-2',
      callType: 'Video',
    });
    expect(session).toMatchObject({
      callId: 'group-call',
      groupId: 'group-1',
      status: 'connecting',
      mode: 'group',
    });
    expect(session.participants?.map((participant) => participant.peerId)).toEqual([
      'peer-6',
      'peer-7',
    ]);
  });

  it('joins a group call by answering existing offers and creating new group offers', async () => {
    const { service } = createService({ callId: 'group-join' });

    await service.handleOffer('group-join', 'peer-8', 'offer-sdp-existing', 'Video');
    const session = await service.joinGroupCall('group-join', 'group-2', ['peer-9'], 'video');

    expect(MockRTCPeerConnection.instances).toHaveLength(2);
    expect(invokeMock).toHaveBeenNthCalledWith(1, 'send_call_join', {
      groupId: 'group-2',
      callId: 'group-join',
    });
    expect(invokeMock).toHaveBeenCalledWith('send_call_answer', {
      peerId: 'peer-8',
      callId: 'group-join',
      sdp: 'answer-sdp-1',
    });
    expect(invokeMock).toHaveBeenCalledWith('send_call_offer', {
      peerId: 'peer-9',
      callId: 'group-join',
      sdp: 'offer-sdp-2',
      callType: 'Video',
    });
    expect(session).toMatchObject({
      callId: 'group-join',
      groupId: 'group-2',
      status: 'connecting',
      mode: 'group',
    });
    expect(
      session.participants?.map((participant) => [participant.peerId, participant.status])
    ).toEqual([
      ['peer-8', 'connecting'],
      ['peer-9', 'connecting'],
    ]);
  });

  it('cleans up only the leaving group participant and keeps the group call alive', async () => {
    const { service } = createService({ callId: 'group-leave' });

    const session = await service.startGroupCall('group-3', ['peer-10', 'peer-11'], 'video');
    const [firstPeerConnection, secondPeerConnection] = MockRTCPeerConnection.instances;
    const localTracks = streamTracks(session.localStream!);

    await service.handleRemoteHangup(session.callId, 'peer-10');

    expect(firstPeerConnection.close).toHaveBeenCalledTimes(1);
    expect(secondPeerConnection.close).not.toHaveBeenCalled();
    expect(localTracks.every((track) => track.stop.mock.calls.length === 0)).toBe(true);
    expect(service.getSession(session.callId)).toMatchObject({
      callId: 'group-leave',
      status: 'connecting',
      mode: 'group',
    });
    expect(
      service.getSession(session.callId)?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-11']);

    await service.handleRemoteHangup(session.callId, 'peer-11');

    expect(secondPeerConnection.close).toHaveBeenCalledTimes(1);
    expect(localTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      localStream: null,
      remoteStream: null,
    });
  });

  it('leaves a group call by sending call leave signaling once and cleaning local resources', async () => {
    const { service } = createService({ callId: 'group-leave-local' });

    const session = await service.startGroupCall('group-4', ['peer-20', 'peer-21'], 'video');
    const [firstPeerConnection, secondPeerConnection] = MockRTCPeerConnection.instances;
    const localTracks = streamTracks(session.localStream!);

    await service.leaveGroupCall(session.callId);

    expect(invokeMock).toHaveBeenCalledWith('send_call_leave', {
      callId: 'group-leave-local',
    });
    expect(firstPeerConnection.close).toHaveBeenCalledTimes(1);
    expect(secondPeerConnection.close).toHaveBeenCalledTimes(1);
    expect(localTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      localStream: null,
      remoteStream: null,
    });
  });

  it('handles inbound call join by creating an offer for the new participant', async () => {
    const { service } = createService({ callId: 'group-join-event' });

    await service.startGroupCall('group-5', ['peer-30'], 'video');
    const session = await service.handleCallJoin(
      'group-join-event',
      'peer-31',
      'group-5',
      true,
      'video'
    );

    expect(MockRTCPeerConnection.instances).toHaveLength(2);
    expect(invokeMock).toHaveBeenLastCalledWith('send_call_offer', {
      peerId: 'peer-31',
      callId: 'group-join-event',
      sdp: 'offer-sdp-2',
      callType: 'Video',
    });
    expect(session.participants?.map((participant) => participant.peerId)).toEqual([
      'peer-30',
      'peer-31',
    ]);
  });

  it('enforces group call capacity for a fifth participant', async () => {
    const { service } = createService({ callId: 'capacity-call' });

    const session = await service.startGroupCall(
      'group-6',
      ['peer-12', 'peer-13', 'peer-14'],
      'video'
    );

    await expect(
      service.joinGroupCall(session.callId, 'group-6', ['peer-15'], 'video')
    ).rejects.toThrow(GROUP_CALL_CAPACITY_ERROR);
    expect(MockRTCPeerConnection.instances).toHaveLength(3);
    expect(
      service.getSession(session.callId)?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-12', 'peer-13', 'peer-14']);
  });

  it('hangs up by stopping tracks, clearing bound elements, and closing the peer connection', async () => {
    const { service } = createService({ callId: 'hangup-call' });
    const localVideo = document.createElement('video');
    const remoteVideo = document.createElement('video');
    const session = await service.startCall('peer-4', 'audio');
    const peerConnection = MockRTCPeerConnection.instances[0];

    service.attachLocalElement(session.callId, localVideo);
    service.attachRemoteElement(session.callId, remoteVideo);

    const remoteTrack = createTrack('audio', 'remote-audio');
    const remoteStream = createStream([remoteTrack]);
    peerConnection.emitTrack(remoteTrack, [remoteStream]);

    const localTracks = streamTracks(session.localStream!);
    const remoteTracks = streamTracks(remoteStream);

    await service.hangup(session.callId);

    expect(invokeMock).toHaveBeenLastCalledWith('send_call_hangup', {
      peerId: 'peer-4',
      callId: 'hangup-call',
    });
    expect(peerConnection.close).toHaveBeenCalledTimes(1);
    expect(localTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(remoteTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(
      (localVideo as HTMLVideoElement & { srcObject?: MediaStream | null }).srcObject
    ).toBeNull();
    expect(
      (remoteVideo as HTMLVideoElement & { srcObject?: MediaStream | null }).srcObject
    ).toBeNull();
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      localStream: null,
      remoteStream: null,
    });
  });

  it('disconnect failure cleanup closes local media when the peer connection enters the failed state', async () => {
    const { service } = createService({ callId: 'failure-call' });
    const session = await service.startCall('peer-5', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];
    const localTracks = streamTracks(session.localStream!);

    peerConnection.setConnectionState('failed');
    await flushTasks();

    expect(peerConnection.close).toHaveBeenCalledTimes(1);
    expect(localTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      lastError: 'Call disconnected',
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it('times out unanswered outgoing calls after 30 seconds', async () => {
    vi.useFakeTimers();

    const { service } = createService({ callId: 'timeout-call' });
    const session = await service.startCall('peer-timeout', 'video');
    const localTracks = streamTracks(session.localStream!);

    await vi.advanceTimersByTimeAsync(CALL_NO_ANSWER_TIMEOUT_MS);

    expect(invokeMock).toHaveBeenLastCalledWith('send_call_hangup', {
      peerId: 'peer-timeout',
      callId: 'timeout-call',
    });
    expect(localTracks.every((track) => track.stop.mock.calls.length === 1)).toBe(true);
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      lastError: 'No answer',
    });
  });

  it('disconnect recovery clears the reconnect window when ICE reconnects in time', async () => {
    vi.useFakeTimers();

    const { service } = createService({ callId: 'disconnect-recover' });
    const session = await service.startCall('peer-recover', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];

    await service.handleAnswer(session.callId, 'answer-sdp-recover');
    peerConnection.setConnectionState('connected');
    await flushTasks();

    peerConnection.setIceConnectionState('disconnected');
    await flushTasks();
    expect(service.getSession(session.callId)).toMatchObject({ status: 'disconnected' });

    await vi.advanceTimersByTimeAsync(CALL_DISCONNECT_TIMEOUT_MS - 1);
    expect(service.getSession(session.callId)).toMatchObject({ status: 'disconnected' });

    peerConnection.setIceConnectionState('connected');
    await flushTasks();
    expect(service.getSession(session.callId)).toMatchObject({ status: 'connected' });

    await vi.advanceTimersByTimeAsync(1);
    expect(service.getSession(session.callId)).toMatchObject({ status: 'connected' });
  });

  it('disconnect timeout ends the call gracefully after 10 seconds', async () => {
    vi.useFakeTimers();

    const { service } = createService({ callId: 'disconnect-timeout' });
    const session = await service.startCall('peer-disconnect', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];

    await service.handleAnswer(session.callId, 'answer-sdp-disconnect');
    peerConnection.setConnectionState('connected');
    await flushTasks();

    peerConnection.setIceConnectionState('disconnected');
    await flushTasks();

    await vi.advanceTimersByTimeAsync(CALL_DISCONNECT_TIMEOUT_MS);

    expect(invokeMock).toHaveBeenLastCalledWith('send_call_hangup', {
      peerId: 'peer-disconnect',
      callId: 'disconnect-timeout',
    });
    expect(service.getSession(session.callId)).toMatchObject({
      status: 'closed',
      lastError: 'Call disconnected',
    });
  });

  it('renegotiation sends a fresh offer when negotiationneeded fires on a connected call', async () => {
    const { service } = createService({ callId: 'renegotiation-offer' });
    const session = await service.startCall('peer-renegotiate', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];

    await service.handleAnswer(session.callId, 'answer-sdp-renegotiate');
    peerConnection.setConnectionState('connected');
    await flushTasks();

    invokeMock.mockClear();
    peerConnection.emitNegotiationNeeded();
    await flushTasks();

    expect(peerConnection.createOffer).toHaveBeenLastCalledWith({ iceRestart: true });
    expect(invokeMock).toHaveBeenCalledWith('send_call_offer', {
      peerId: 'peer-renegotiate',
      callId: 'renegotiation-offer',
      sdp: 'offer-sdp-1-2',
      callType: 'Video',
    });
  });

  it('renegotiation auto-answers a re-offer on an active call', async () => {
    const { service } = createService({ callId: 'renegotiation-answer' });
    const session = await service.startCall('peer-reoffer', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];

    await service.handleAnswer(session.callId, 'answer-sdp-initial');
    peerConnection.setConnectionState('connected');
    await flushTasks();

    invokeMock.mockClear();
    await service.handleOffer(session.callId, 'peer-reoffer', 'offer-sdp-reoffer', 'Video');

    expect(peerConnection.setRemoteDescription).toHaveBeenLastCalledWith({
      type: 'offer',
      sdp: 'offer-sdp-reoffer',
    });
    expect(invokeMock).toHaveBeenCalledWith('send_call_answer', {
      peerId: 'peer-reoffer',
      callId: 'renegotiation-answer',
      sdp: 'answer-sdp-1',
    });
    expect(service.getSession(session.callId)).toMatchObject({ status: 'connected' });
  });

  it('toggles audio mute state', async () => {
    const { service } = createService();
    const session = await service.startCall('peer-1', 'audio');

    expect(session.localStream?.getAudioTracks()[0]?.enabled).toBe(true);
    service.toggleMute(session.callId);
    expect(session.localStream?.getAudioTracks()[0]?.enabled).toBe(false);
    service.toggleMute(session.callId);
    expect(session.localStream?.getAudioTracks()[0]?.enabled).toBe(true);
  });

  it('toggles video camera state', async () => {
    const { service } = createService();
    const session = await service.startCall('peer-1', 'video');

    expect(session.localStream?.getVideoTracks()[0]?.enabled).toBe(true);
    service.toggleCamera(session.callId);
    expect(session.localStream?.getVideoTracks()[0]?.enabled).toBe(false);
    service.toggleCamera(session.callId);
    expect(session.localStream?.getVideoTracks()[0]?.enabled).toBe(true);
  });

  it('switches audio device', async () => {
    const { service, getUserMediaMock } = createService();
    const session = await service.startCall('peer-1', 'audio');

    const oldTrack = session.localStream?.getAudioTracks()[0];

    await service.switchAudioDevice(session.callId, 'new-audio-id');

    expect(getUserMediaMock).toHaveBeenCalledWith({
      audio: { deviceId: { exact: 'new-audio-id' } },
    });

    const newTrack = session.localStream?.getAudioTracks()[0];
    expect(newTrack).not.toBe(oldTrack);
    // @ts-ignore
    expect(oldTrack?.stop).toHaveBeenCalled();
  });

  it('switches video device', async () => {
    const { service, getUserMediaMock } = createService();
    const session = await service.startCall('peer-1', 'video');

    const oldTrack = session.localStream?.getVideoTracks()[0];

    await service.switchVideoDevice(session.callId, 'new-video-id');

    expect(getUserMediaMock).toHaveBeenCalledWith({
      video: { deviceId: { exact: 'new-video-id' } },
    });

    const newTrack = session.localStream?.getVideoTracks()[0];
    expect(newTrack).not.toBe(oldTrack);
    // @ts-ignore
    expect(oldTrack?.stop).toHaveBeenCalled();
  });

  it('starts screen sharing by replacing the current video track during an active call', async () => {
    const { service, getDisplayMediaMock } = createService({ callId: 'screen-start' });
    const session = await service.startCall('peer-screen', 'video');
    const peerConnection = MockRTCPeerConnection.instances[0];
    const cameraTrack = session.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;

    await service.startScreenShare(session.callId);

    const nextSession = service.getSession(session.callId)!;
    const screenTrack =
      nextSession.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;
    const videoSender = peerConnection.senders.find((sender) => sender.track.kind === 'video');

    expect(getDisplayMediaMock).toHaveBeenCalledWith({ video: true });
    expect(nextSession.isScreenSharing).toBe(true);
    expect(screenTrack).not.toBe(cameraTrack);
    expect(videoSender?.replaceTrack).toHaveBeenCalledWith(screenTrack);
    expect(cameraTrack.stop).not.toHaveBeenCalled();
  });

  it('stops screen sharing by restoring the camera track for every group peer connection', async () => {
    const { service } = createService({ callId: 'screen-group' });
    const session = await service.startGroupCall('group-screen', ['peer-a', 'peer-b'], 'video');
    const [firstPeerConnection, secondPeerConnection] = MockRTCPeerConnection.instances;
    const firstSender = firstPeerConnection.senders.find((sender) => sender.track.kind === 'video');
    const secondSender = secondPeerConnection.senders.find(
      (sender) => sender.track.kind === 'video'
    );
    const cameraTrack = session.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;

    await service.startScreenShare(session.callId);
    const screenTrack = service
      .getSession(session.callId)
      ?.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;

    expect(firstSender?.replaceTrack).toHaveBeenCalledWith(screenTrack);
    expect(secondSender?.replaceTrack).toHaveBeenCalledWith(screenTrack);

    await service.stopScreenShare(session.callId);

    const restoredSession = service.getSession(session.callId)!;
    expect(restoredSession.isScreenSharing).toBe(false);
    expect(restoredSession.localStream?.getVideoTracks()[0]).toBe(cameraTrack);
    expect(screenTrack.stop).toHaveBeenCalledTimes(1);
    expect(firstSender?.replaceTrack).toHaveBeenLastCalledWith(cameraTrack);
    expect(secondSender?.replaceTrack).toHaveBeenLastCalledWith(cameraTrack);
  });

  it('restores the camera when the native screen-share track ends', async () => {
    const { service } = createService({ callId: 'screen-ended' });
    const session = await service.startCall('peer-ended', 'video');
    const cameraTrack = session.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;

    await service.startScreenShare(session.callId);

    const screenTrack = service
      .getSession(session.callId)
      ?.localStream?.getVideoTracks()[0] as unknown as MockMediaStreamTrack;
    screenTrack.emitEnded();
    await flushTasks();

    const restoredSession = service.getSession(session.callId)!;
    expect(restoredSession.isScreenSharing).toBe(false);
    expect(restoredSession.localStream?.getVideoTracks()[0]).toBe(cameraTrack);
    expect(screenTrack.stop).not.toHaveBeenCalled();
  });
});
