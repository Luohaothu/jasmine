import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createCallStore, setupCallEventListeners, type CallStoreDependencies } from './callStore';
import {
  CALL_NO_ANSWER_TIMEOUT_MS,
  GROUP_CALL_CAPACITY_ERROR,
  type CallParticipant,
  type CallType,
  type SignalingCallType,
  type SignalingIceCandidate,
  type WebRTCCallSession,
} from '../services/webrtc';

type ListenerCallback = (event: { payload: unknown }) => void | Promise<void>;

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

function deriveStatus(participants: CallParticipant[]): WebRTCCallSession['status'] {
  const statuses = participants.map((participant) => participant.status);
  if (statuses.length === 0) {
    return 'closed';
  }

  if (statuses.includes('connected')) {
    return 'connected';
  }

  if (statuses.includes('connecting')) {
    return 'connecting';
  }

  if (statuses.includes('new')) {
    return 'new';
  }

  return 'disconnected';
}

function makeParticipant(
  peerId: string,
  status: CallParticipant['status'],
  overrides: Partial<CallParticipant> = {}
): CallParticipant {
  return {
    peerId,
    status,
    remoteStream: overrides.remoteStream ?? null,
    lastError: overrides.lastError,
  };
}

function makeSession(overrides: Partial<WebRTCCallSession> = {}): WebRTCCallSession {
  const participants = overrides.participants ?? [
    makeParticipant(overrides.peerId ?? 'peer-1', overrides.status ?? 'new'),
  ];
  const peerId = overrides.peerId ?? participants[0]?.peerId ?? '';
  const primaryParticipant = participants.find((participant) => participant.peerId === peerId);

  return {
    callId: overrides.callId ?? 'call-1',
    groupId: overrides.groupId,
    peerId,
    callType: overrides.callType ?? 'video',
    direction: overrides.direction ?? 'outgoing',
    status: overrides.status ?? deriveStatus(participants),
    localStream: overrides.localStream ?? null,
    remoteStream: overrides.remoteStream ?? primaryParticipant?.remoteStream ?? null,
    isScreenSharing: overrides.isScreenSharing ?? false,
    lastError: overrides.lastError,
    mode: overrides.mode ?? (participants.length > 1 ? 'group' : 'direct'),
    participants,
  };
}

function upsertParticipant(
  participants: CallParticipant[],
  participant: CallParticipant
): CallParticipant[] {
  const existingIndex = participants.findIndex(
    (candidate) => candidate.peerId === participant.peerId
  );
  if (existingIndex === -1) {
    return [...participants, participant];
  }

  return participants.map((candidate, index) =>
    index === existingIndex ? participant : candidate
  );
}

function removeParticipant(participants: CallParticipant[], peerId: string): CallParticipant[] {
  return participants.filter((participant) => participant.peerId !== peerId);
}

function createMockService() {
  const listeners = new Set<(session: WebRTCCallSession) => void>();
  const sessions = new Map<string, WebRTCCallSession>();
  let nextOutgoingCallId = 1;

  const emitSession = (session: WebRTCCallSession): WebRTCCallSession => {
    const normalizedSession = makeSession(session);

    if (normalizedSession.status === 'closed') {
      sessions.delete(normalizedSession.callId);
    } else {
      sessions.set(normalizedSession.callId, normalizedSession);
    }

    for (const listener of listeners) {
      listener(normalizedSession);
    }

    return normalizedSession;
  };

  const requireSession = (callId: string): WebRTCCallSession => {
    const session = sessions.get(callId);
    if (!session) {
      throw new Error(`Unknown call: ${callId}`);
    }

    return session;
  };

  const updateSession = (
    callId: string,
    updater: (session: WebRTCCallSession) => WebRTCCallSession
  ): WebRTCCallSession => {
    const currentSession = requireSession(callId);
    return emitSession(updater(currentSession));
  };

  const closeSession = (callId: string): void => {
    const session = requireSession(callId);
    emitSession(
      makeSession({
        ...session,
        status: 'closed',
        localStream: null,
        remoteStream: null,
        participants: [],
      })
    );
  };

  return {
    emitSession,
    service: {
      subscribe: vi.fn((listener: (session: WebRTCCallSession) => void) => {
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      }),

      startCall: vi.fn(async (peerId: string, callType: CallType) => {
        return emitSession(
          makeSession({
            callId: `outgoing-${nextOutgoingCallId++}`,
            peerId,
            callType,
            direction: 'outgoing',
            status: 'connecting',
            mode: 'direct',
            participants: [makeParticipant(peerId, 'connecting')],
          })
        );
      }),

      startGroupCall: vi.fn(async (groupId: string, peerIds: string[], callType: CallType) => {
        return emitSession(
          makeSession({
            callId: `outgoing-${nextOutgoingCallId++}`,
            groupId,
            peerId: peerIds[0] ?? '',
            callType,
            direction: 'outgoing',
            status: 'connecting',
            mode: 'group',
            participants: peerIds.map((peerId) => makeParticipant(peerId, 'connecting')),
          })
        );
      }),

      joinGroupCall: vi.fn(
        async (
          callId: string,
          groupId: string | undefined,
          peerIds: string[],
          callType: CallType = 'video'
        ) => {
          const existingSession = sessions.get(callId);
          const existingParticipants = existingSession?.participants ?? [];
          const nextPeerIds = peerIds.filter(
            (peerId, index) =>
              peerIds.indexOf(peerId) === index &&
              !existingParticipants.some((participant) => participant.peerId === peerId)
          );

          if (existingParticipants.length + nextPeerIds.length > 3) {
            throw new Error(GROUP_CALL_CAPACITY_ERROR);
          }

          const participants = [
            ...existingParticipants.map((participant) =>
              participant.status === 'new'
                ? makeParticipant(participant.peerId, 'connecting', participant)
                : participant
            ),
            ...nextPeerIds.map((peerId) => makeParticipant(peerId, 'connecting')),
          ];

          return emitSession(
            makeSession({
              ...existingSession,
              callId,
              groupId: groupId ?? existingSession?.groupId,
              peerId: participants[0]?.peerId ?? existingSession?.peerId ?? '',
              callType,
              direction: existingSession?.direction ?? 'incoming',
              status: 'connecting',
              mode: 'group',
              participants,
            })
          );
        }
      ),

      handleCallJoin: vi.fn(
        async (
          callId: string,
          peerId: string,
          groupId: string,
          createOffer = false,
          callType: CallType = 'video'
        ) => {
          const existingSession = sessions.get(callId);
          const existingParticipants = existingSession?.participants ?? [];
          const isKnownParticipant = existingParticipants.some(
            (participant) => participant.peerId === peerId
          );

          if (!isKnownParticipant && existingParticipants.length >= 3) {
            throw new Error(GROUP_CALL_CAPACITY_ERROR);
          }

          const participants = upsertParticipant(
            existingParticipants,
            makeParticipant(peerId, createOffer ? 'connecting' : 'new')
          );

          return emitSession(
            makeSession({
              ...existingSession,
              callId,
              groupId,
              peerId: existingSession?.peerId ?? peerId,
              callType: existingSession?.callType ?? callType,
              direction: existingSession?.direction ?? 'incoming',
              status: deriveStatus(participants),
              mode: 'group',
              participants,
            })
          );
        }
      ),

      leaveGroupCall: vi.fn(async (callId: string) => {
        closeSession(callId);
      }),

      handleOffer: vi.fn(
        async (
          callId: string,
          peerId: string,
          _sdp: string,
          callType: CallType | SignalingCallType = 'video'
        ) => {
          const existingSession = sessions.get(callId);
          const existingParticipants = existingSession?.participants ?? [];
          const isKnownParticipant = existingParticipants.some(
            (participant) => participant.peerId === peerId
          );

          if (!isKnownParticipant && existingParticipants.length >= 3) {
            throw new Error(GROUP_CALL_CAPACITY_ERROR);
          }

          return emitSession(
            makeSession({
              ...existingSession,
              callId,
              peerId: existingSession?.peerId ?? peerId,
              callType: normalizeCallType(callType),
              direction: existingSession?.direction ?? 'incoming',
              mode:
                existingParticipants.length > 0 || existingSession?.mode === 'group'
                  ? 'group'
                  : 'direct',
              participants: upsertParticipant(existingParticipants, makeParticipant(peerId, 'new')),
            })
          );
        }
      ),

      answerCall: vi.fn(async (callId: string, peerId: string) => {
        return updateSession(callId, (session) =>
          makeSession({
            ...session,
            peerId,
            status: 'connecting',
            participants: session.participants?.map((participant) =>
              participant.peerId === peerId
                ? makeParticipant(peerId, 'connecting', participant)
                : participant
            ),
          })
        );
      }),

      handleAnswer: vi.fn(async (callId: string, _sdp: string, peerId?: string) => {
        return updateSession(callId, (session) => {
          const targetPeerId = peerId ?? session.peerId;
          return makeSession({
            ...session,
            status: 'connected',
            participants: session.participants?.map((participant) =>
              participant.peerId === targetPeerId
                ? makeParticipant(targetPeerId, 'connected', participant)
                : participant
            ),
          });
        });
      }),

      handleIceCandidate: vi.fn(async (callId: string, _candidate: SignalingIceCandidate) => {
        return requireSession(callId);
      }),

      startScreenShare: vi.fn(async (callId: string) => {
        return updateSession(callId, (session) =>
          makeSession({
            ...session,
            isScreenSharing: true,
          })
        );
      }),

      stopScreenShare: vi.fn(async (callId: string) => {
        return updateSession(callId, (session) =>
          makeSession({
            ...session,
            isScreenSharing: false,
          })
        );
      }),

      hangup: vi.fn(async (callId: string) => {
        closeSession(callId);
      }),

      handleRemoteHangup: vi.fn(async (callId: string, peerId?: string) => {
        const session = sessions.get(callId);
        if (!session) {
          return;
        }

        if (!peerId) {
          closeSession(callId);
          return;
        }

        const remainingParticipants = removeParticipant(session.participants ?? [], peerId);
        if (remainingParticipants.length === 0) {
          closeSession(callId);
          return;
        }

        emitSession(
          makeSession({
            ...session,
            peerId: remainingParticipants[0]?.peerId ?? '',
            status: deriveStatus(remainingParticipants),
            participants: remainingParticipants,
            mode: session.mode,
          })
        );
      }),

      getMediaDevices: vi.fn(async () => []),
      toggleMute: vi.fn(),
      toggleCamera: vi.fn(),
      switchAudioDevice: vi.fn(async () => {}),
      switchVideoDevice: vi.fn(async () => {}),
    },
  };
}

async function flushTasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe('callStore', () => {
  const listeners = new Map<string, ListenerCallback>();
  let cleanup: (() => void) | undefined;

  beforeEach(async () => {
    vi.useRealTimers();
    listeners.clear();
  });

  afterEach(() => {
    vi.useRealTimers();
    cleanup?.();
    cleanup = undefined;
  });

  async function createHarness() {
    const mockService = createMockService();
    const listenMock: CallStoreDependencies['listen'] = async <T>(
      eventName: string,
      callback: (event: { payload: T }) => void | Promise<void>
    ) => {
      listeners.set(eventName, callback as unknown as ListenerCallback);
      return () => {
        listeners.delete(eventName);
      };
    };

    const dependencies: CallStoreDependencies = {
      service: mockService.service,
      listen: listenMock,
      rejectCall: vi.fn(async () => undefined),
    };
    const store = createCallStore(dependencies);
    cleanup = await setupCallEventListeners(store, dependencies);

    return {
      dependencies,
      mockService,
      store,
    };
  }

  it('starts outgoing calls and keeps active call state in sync with service updates', async () => {
    const { dependencies, mockService, store } = await createHarness();

    const session = await store.getState().startCall('peer-1', 'video');
    expect(dependencies.service.startCall).toHaveBeenCalledWith('peer-1', 'video');
    expect(store.getState().incomingCall).toBeNull();
    expect(store.getState().activeCall).toMatchObject({
      callId: session.callId,
      peerId: 'peer-1',
      callType: 'video',
      direction: 'outgoing',
      status: 'connecting',
    });

    mockService.emitSession(
      makeSession({
        ...session,
        status: 'connected',
        participants: [makeParticipant('peer-1', 'connected')],
      })
    );
    await flushTasks();

    expect(store.getState().activeCall?.status).toBe('connected');
  });

  it('builds incoming call state from offer events and accepts through the WebRTC service', async () => {
    const { dependencies, store } = await createHarness();

    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-2',
        callId: 'incoming-1',
        sdp: 'offer-sdp',
        callerId: 'peer-2',
        callType: 'Audio',
      },
    });
    await flushTasks();

    expect(dependencies.service.handleOffer).toHaveBeenCalledWith(
      'incoming-1',
      'peer-2',
      'offer-sdp',
      'Audio'
    );
    expect(store.getState().incomingCall).toEqual({
      callId: 'incoming-1',
      peerId: 'peer-2',
      callerId: 'peer-2',
      callType: 'audio',
      status: 'new',
      mode: 'direct',
      participants: [makeParticipant('peer-2', 'new')],
    });

    const session = await store.getState().acceptIncomingCall();

    expect(dependencies.service.answerCall).toHaveBeenCalledWith('incoming-1', 'peer-2');
    expect(session).toMatchObject({
      callId: 'incoming-1',
      direction: 'incoming',
      status: 'connecting',
    });
    expect(store.getState().incomingCall).toBeNull();
    expect(store.getState().activeCall).toMatchObject({
      callId: 'incoming-1',
      peerId: 'peer-2',
      callType: 'audio',
      direction: 'incoming',
      status: 'connecting',
    });
  });

  it('rejects incoming calls and clears pending state', async () => {
    const { dependencies, store } = await createHarness();

    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-3',
        callId: 'incoming-2',
        sdp: 'offer-sdp',
        callerId: 'peer-3',
        callType: 'Video',
      },
    });
    await flushTasks();

    await store.getState().rejectIncomingCall('busy');

    expect(dependencies.rejectCall).toHaveBeenCalledWith('peer-3', 'incoming-2', 'busy');
    expect(dependencies.service.handleRemoteHangup).toHaveBeenCalledWith('incoming-2');
    expect(store.getState().incomingCall).toBeNull();
    expect(store.getState().activeCall).toBeNull();
  });

  it('auto-declines unanswered incoming calls after 30 seconds', async () => {
    vi.useFakeTimers();

    const { dependencies, store } = await createHarness();

    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-timeout',
        callId: 'incoming-timeout',
        sdp: 'offer-timeout',
        callerId: 'peer-timeout',
        callType: 'Video',
      },
    });
    await flushTasks();

    await vi.advanceTimersByTimeAsync(CALL_NO_ANSWER_TIMEOUT_MS);

    expect(dependencies.rejectCall).toHaveBeenCalledWith(
      'peer-timeout',
      'incoming-timeout',
      'no-answer'
    );
    expect(dependencies.service.handleRemoteHangup).toHaveBeenCalledWith('incoming-timeout');
    expect(store.getState().incomingCall).toBeNull();
    expect(store.getState().activeCall).toBeNull();
  });

  it('hangs up active calls and cleans store state', async () => {
    const { dependencies, store } = await createHarness();

    const session = await store.getState().startCall('peer-4', 'audio');
    await store.getState().hangupActiveCall();

    expect(dependencies.service.hangup).toHaveBeenCalledWith(session.callId);
    expect(store.getState().activeCall).toBeNull();
    expect(store.getState().incomingCall).toBeNull();
  });

  it('handles rapid answer, ICE, and hangup events without leaving stale call state', async () => {
    const { dependencies, store } = await createHarness();

    const session = await store.getState().startCall('peer-5', 'video');

    listeners.get('call-answer')?.({
      payload: {
        peerId: 'peer-5',
        callId: session.callId,
        sdp: 'answer-sdp',
      },
    });
    await flushTasks();

    listeners.get('ice-candidate')?.({
      payload: {
        peerId: 'peer-5',
        callId: session.callId,
        candidate: 'candidate:1',
        sdpMid: '0',
        sdpMlineIndex: 0,
      },
    });
    await flushTasks();

    listeners.get('call-hangup')?.({
      payload: {
        peerId: 'peer-5',
        callId: session.callId,
      },
    });
    await flushTasks();

    listeners.get('call-reject')?.({
      payload: {
        peerId: 'peer-5',
        callId: session.callId,
        reason: 'late-reject',
      },
    });
    await flushTasks();

    expect(dependencies.service.handleAnswer).toHaveBeenCalledWith(
      session.callId,
      'answer-sdp',
      'peer-5'
    );
    expect(dependencies.service.handleIceCandidate).toHaveBeenCalledWith(
      session.callId,
      {
        candidate: 'candidate:1',
        sdpMid: '0',
        sdpMlineIndex: 0,
      },
      'peer-5'
    );
    expect(dependencies.service.handleRemoteHangup).toHaveBeenCalledTimes(1);
    expect(dependencies.service.handleRemoteHangup).toHaveBeenCalledWith(session.callId, 'peer-5');
    expect(store.getState().activeCall).toBeNull();
    expect(store.getState().incomingCall).toBeNull();
  });

  it('keeps disconnected calls active until the service reports recovery or closure', async () => {
    const { mockService, store } = await createHarness();

    const session = await store.getState().startCall('peer-disconnect', 'video');

    mockService.emitSession(
      makeSession({
        ...session,
        status: 'disconnected',
        participants: [makeParticipant('peer-disconnect', 'disconnected')],
      })
    );
    await flushTasks();

    expect(store.getState().activeCall).toMatchObject({
      callId: session.callId,
      status: 'disconnected',
    });

    mockService.emitSession(
      makeSession({
        ...session,
        status: 'connected',
        participants: [makeParticipant('peer-disconnect', 'connected')],
      })
    );
    await flushTasks();

    expect(store.getState().activeCall).toMatchObject({
      callId: session.callId,
      status: 'connected',
    });
  });

  it('toggles screen sharing only while an active call exists', async () => {
    const { mockService, store } = await createHarness();

    await store.getState().toggleScreenShare();
    expect(mockService.service.startScreenShare).not.toHaveBeenCalled();

    const session = await store.getState().startCall('peer-screen', 'video');

    await store.getState().toggleScreenShare();
    expect(mockService.service.startScreenShare).toHaveBeenCalledWith(session.callId);
    expect(store.getState().isScreenSharing).toBe(true);

    await store.getState().toggleScreenShare();
    expect(mockService.service.stopScreenShare).toHaveBeenCalledWith(session.callId);
    expect(store.getState().isScreenSharing).toBe(false);
  });

  it('syncs screen-share state from service updates when native sharing ends', async () => {
    const { mockService, store } = await createHarness();

    const session = await store.getState().startCall('peer-sync', 'video');
    await store.getState().toggleScreenShare();

    mockService.emitSession(
      makeSession({
        ...session,
        status: 'connected',
        isScreenSharing: false,
      })
    );
    await flushTasks();

    expect(store.getState().activeCall?.callId).toBe(session.callId);
    expect(store.getState().isScreenSharing).toBe(false);
  });

  it('starts group calls and keeps group participants in sync with service updates', async () => {
    const { dependencies, mockService, store } = await createHarness();

    const session = await store.getState().startGroupCall('group-1', ['peer-6', 'peer-7'], 'video');

    expect(dependencies.service.startGroupCall).toHaveBeenCalledWith(
      'group-1',
      ['peer-6', 'peer-7'],
      'video'
    );
    expect(store.getState().activeCall).toMatchObject({
      callId: session.callId,
      groupId: 'group-1',
      mode: 'group',
      status: 'connecting',
    });
    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-6', 'peer-7']);

    mockService.emitSession(
      makeSession({
        ...session,
        mode: 'group',
        status: 'connecting',
        participants: [
          makeParticipant('peer-6', 'connected'),
          makeParticipant('peer-7', 'connected'),
          makeParticipant('peer-8', 'connecting'),
        ],
      })
    );
    await flushTasks();

    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-6', 'peer-7', 'peer-8']);
  });

  it('accepts grouped incoming offers by joining all pending participants', async () => {
    const { dependencies, store } = await createHarness();

    listeners.get('call-join')?.({
      payload: {
        peerId: 'peer-9',
        callId: 'group-incoming',
        groupId: 'group-2',
      },
    });
    await flushTasks();

    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-9',
        callId: 'group-incoming',
        sdp: 'offer-a',
        callerId: 'peer-9',
        callType: 'Video',
      },
    });
    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-10',
        callId: 'group-incoming',
        sdp: 'offer-b',
        callerId: 'peer-9',
        callType: 'Video',
      },
    });
    await flushTasks();

    expect(store.getState().incomingCall).toMatchObject({
      callId: 'group-incoming',
      groupId: 'group-2',
      mode: 'group',
    });
    expect(
      store.getState().incomingCall?.participants.map((participant) => participant.peerId)
    ).toEqual(['peer-9', 'peer-10']);

    const session = await store.getState().acceptIncomingCall();

    expect(dependencies.service.joinGroupCall).toHaveBeenCalledWith(
      'group-incoming',
      'group-2',
      ['peer-9', 'peer-10'],
      'video'
    );
    expect(session).toMatchObject({
      callId: 'group-incoming',
      mode: 'group',
      status: 'connecting',
    });
    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-9', 'peer-10']);
  });

  it('handles call-join and call-leave events for an active group call', async () => {
    const { dependencies, store } = await createHarness();

    const session = await store
      .getState()
      .startGroupCall('group-3', ['peer-11', 'peer-12'], 'video');

    listeners.get('call-join')?.({
      payload: {
        peerId: 'peer-13',
        callId: session.callId,
        groupId: 'group-3',
      },
    });
    await flushTasks();

    expect(dependencies.service.handleCallJoin).toHaveBeenCalledWith(
      session.callId,
      'peer-13',
      'group-3',
      true,
      'video'
    );
    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-11', 'peer-12', 'peer-13']);

    listeners.get('call-leave')?.({
      payload: {
        peerId: 'peer-11',
        callId: session.callId,
      },
    });
    await flushTasks();

    expect(dependencies.service.handleRemoteHangup).toHaveBeenCalledWith(session.callId, 'peer-11');
    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-12', 'peer-13']);

    listeners.get('call-leave')?.({
      payload: {
        peerId: 'peer-12',
        callId: session.callId,
      },
    });
    await flushTasks();

    listeners.get('call-leave')?.({
      payload: {
        peerId: 'peer-13',
        callId: session.callId,
      },
    });
    await flushTasks();

    expect(store.getState().activeCall).toBeNull();
  });

  it('rejects a fifth group participant cleanly', async () => {
    const { dependencies, store } = await createHarness();

    const session = await store
      .getState()
      .startGroupCall('group-4', ['peer-13', 'peer-14', 'peer-15'], 'video');

    listeners.get('call-offer')?.({
      payload: {
        peerId: 'peer-16',
        callId: session.callId,
        sdp: 'offer-over-capacity',
        callerId: 'peer-16',
        callType: 'Video',
      },
    });
    await flushTasks();

    expect(dependencies.rejectCall).toHaveBeenCalledWith('peer-16', session.callId, 'capacity');
    expect(
      store.getState().activeCall?.participants?.map((participant) => participant.peerId)
    ).toEqual(['peer-13', 'peer-14', 'peer-15']);
  });

  describe('device management', () => {
    it('enumerates devices', async () => {
      const { store, mockService } = await createHarness();
      const mockDevices = [{ deviceId: '1', kind: 'audioinput', label: 'Mic' }];
      mockService.service.getMediaDevices.mockResolvedValue(mockDevices as any);

      await store.getState().enumerateDevices();

      expect(mockService.service.getMediaDevices).toHaveBeenCalled();
      expect(store.getState().devices).toEqual(mockDevices);
    });

    it('toggles mute state and calls service', async () => {
      const { store, mockService } = await createHarness();

      await store.getState().startCall('peer-1', 'audio');

      expect(store.getState().isMuted).toBe(false);
      store.getState().toggleMute();
      expect(store.getState().isMuted).toBe(true);
      const state = store.getState();
      expect(mockService.service.toggleMute).toHaveBeenCalledWith(state.activeCall?.callId);
    });

    it('toggles camera state and calls service', async () => {
      const { store, mockService } = await createHarness();

      await store.getState().startCall('peer-1', 'video');

      expect(store.getState().isCameraOff).toBe(false);
      store.getState().toggleCamera();
      expect(store.getState().isCameraOff).toBe(true);
      const state = store.getState();
      expect(mockService.service.toggleCamera).toHaveBeenCalledWith(state.activeCall?.callId);
    });

    it('switches audio device', async () => {
      const { store, mockService } = await createHarness();

      await store.getState().startCall('peer-1', 'audio');

      await store.getState().setAudioDevice('device-1');
      expect(store.getState().selectedAudioDeviceId).toBe('device-1');
      const state = store.getState();
      expect(mockService.service.switchAudioDevice).toHaveBeenCalledWith(
        state.activeCall?.callId,
        'device-1'
      );
    });

    it('switches video device', async () => {
      const { store, mockService } = await createHarness();

      await store.getState().startCall('peer-1', 'video');

      await store.getState().setVideoDevice('device-2');
      expect(store.getState().selectedVideoDeviceId).toBe('device-2');
      const state = store.getState();
      expect(mockService.service.switchVideoDevice).toHaveBeenCalledWith(
        state.activeCall?.callId,
        'device-2'
      );
    });
  });
});
