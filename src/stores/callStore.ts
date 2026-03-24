import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { create, type StoreApi, type UseBoundStore } from 'zustand';

import {
  CALL_NO_ANSWER_TIMEOUT_MS,
  GROUP_CALL_CAPACITY_ERROR,
  WebRTCService,
  type CallParticipant,
  type CallType,
  type SignalingCallType,
  type SignalingIceCandidate,
  type WebRTCCallSession,
} from '../services/webrtc';

export interface CallOfferEventPayload {
  peerId: string;
  callId: string;
  sdp: string;
  callerId: string;
  callType: SignalingCallType;
}

export interface CallAnswerEventPayload {
  peerId: string;
  callId: string;
  sdp: string;
}

export interface IceCandidateEventPayload extends SignalingIceCandidate {
  peerId: string;
  callId: string;
}

export interface CallHangupEventPayload {
  peerId: string;
  callId: string;
}

export interface CallRejectEventPayload {
  peerId: string;
  callId: string;
  reason?: string | null;
}

export interface CallJoinEventPayload {
  peerId: string;
  callId: string;
  groupId: string;
}

export interface CallLeaveEventPayload {
  peerId: string;
  callId: string;
}

export interface IncomingCall {
  callId: string;
  groupId?: string;
  peerId: string;
  callerId: string;
  callType: CallType;
  status: WebRTCCallSession['status'];
  mode?: WebRTCCallSession['mode'];
  participants: CallParticipant[];
}

type CallStoreHook = UseBoundStore<StoreApi<CallStore>>;
type CallEvent<T> = { payload: T };
type CallEventHandler<T> = (event: CallEvent<T>) => void | Promise<void>;
type CallListen = <T>(eventName: string, handler: CallEventHandler<T>) => Promise<() => void>;

interface CallService {
  subscribe: (listener: (session: WebRTCCallSession) => void) => () => void;
  startCall: (peerId: string, callType: CallType) => Promise<WebRTCCallSession>;
  startGroupCall: (
    groupId: string,
    peerIds: string[],
    callType: CallType
  ) => Promise<WebRTCCallSession>;
  joinGroupCall: (
    callId: string,
    groupId: string | undefined,
    peerIds: string[],
    callType?: CallType
  ) => Promise<WebRTCCallSession>;
  handleCallJoin: (
    callId: string,
    peerId: string,
    groupId: string,
    createOffer?: boolean,
    callType?: CallType
  ) => Promise<WebRTCCallSession>;
  leaveGroupCall: (callId: string) => Promise<void>;
  handleOffer: (
    callId: string,
    peerId: string,
    sdp: string,
    callType?: CallType | SignalingCallType
  ) => Promise<WebRTCCallSession>;
  answerCall: (callId: string, peerId: string) => Promise<WebRTCCallSession>;
  handleAnswer: (callId: string, sdp: string, peerId?: string) => Promise<WebRTCCallSession>;
  handleIceCandidate: (
    callId: string,
    candidate: SignalingIceCandidate,
    peerId?: string
  ) => Promise<WebRTCCallSession>;
  hangup: (callId: string) => Promise<void>;
  getMediaDevices: () => Promise<MediaDeviceInfo[]>;
  toggleMute: (callId: string) => void;
  toggleCamera: (callId: string) => void;
  startScreenShare: (callId: string) => Promise<WebRTCCallSession>;
  stopScreenShare: (callId: string) => Promise<WebRTCCallSession>;
  switchAudioDevice: (callId: string, deviceId: string) => Promise<void>;
  switchVideoDevice: (callId: string, deviceId: string) => Promise<void>;
  handleRemoteHangup: (callId: string, peerId?: string) => Promise<void>;
}

export interface CallStoreDependencies {
  service: CallService;
  listen: CallListen;
  rejectCall: (peerId: string, callId: string, reason?: string) => Promise<void>;
}

export type CallStore = {
  activeCall: WebRTCCallSession | null;
  incomingCall: IncomingCall | null;
  devices: MediaDeviceInfo[];
  isMuted: boolean;
  isCameraOff: boolean;
  isScreenSharing: boolean;
  selectedAudioDeviceId: string | null;
  selectedVideoDeviceId: string | null;

  enumerateDevices: () => Promise<void>;
  toggleMute: () => void;
  toggleCamera: () => void;
  toggleScreenShare: () => Promise<void>;
  setAudioDevice: (deviceId: string) => Promise<void>;
  setVideoDevice: (deviceId: string) => Promise<void>;
  startCall: (peerId: string, callType: CallType) => Promise<WebRTCCallSession>;
  startGroupCall: (
    groupId: string,
    peerIds: string[],
    callType: CallType
  ) => Promise<WebRTCCallSession>;
  joinGroupCall: (
    callId: string,
    groupId: string | undefined,
    peerIds: string[],
    callType?: CallType
  ) => Promise<WebRTCCallSession>;
  acceptIncomingCall: () => Promise<WebRTCCallSession | null>;
  rejectIncomingCall: (reason?: string) => Promise<void>;
  hangupActiveCall: () => Promise<void>;
  leaveGroupCall: () => Promise<void>;
  syncSession: (session: WebRTCCallSession) => void;
  handleCallOffer: (payload: CallOfferEventPayload) => Promise<void>;
  handleCallJoin: (payload: CallJoinEventPayload) => Promise<void>;
  handleCallAnswer: (payload: CallAnswerEventPayload) => Promise<void>;
  handleIceCandidate: (payload: IceCandidateEventPayload) => Promise<void>;
  handleCallHangup: (payload: CallHangupEventPayload) => Promise<void>;
  handleCallReject: (payload: CallRejectEventPayload) => Promise<void>;
  handleCallLeave: (payload: CallLeaveEventPayload) => Promise<void>;
};

const defaultDependencies: CallStoreDependencies = {
  service: new WebRTCService(),
  listen,
  rejectCall: async (peerId, callId, reason) => {
    await invoke('send_call_reject', {
      peerId,
      callId,
      reason: reason ?? null,
    });
  },
};

function clearTrackedCall(
  state: Pick<CallStore, 'activeCall' | 'incomingCall' | 'isScreenSharing'>,
  callId: string
) {
  const shouldClearMediaState =
    state.activeCall?.callId === callId || state.incomingCall?.callId === callId;

  return {
    activeCall: state.activeCall?.callId === callId ? null : state.activeCall,
    incomingCall: state.incomingCall?.callId === callId ? null : state.incomingCall,
    isScreenSharing: shouldClearMediaState ? false : state.isScreenSharing,
  };
}

function trackedCallId(state: Pick<CallStore, 'activeCall' | 'incomingCall'>): string | null {
  return state.activeCall?.callId ?? state.incomingCall?.callId ?? null;
}

function isTrackedCall(
  state: Pick<CallStore, 'activeCall' | 'incomingCall'>,
  callId: string
): boolean {
  return state.activeCall?.callId === callId || state.incomingCall?.callId === callId;
}

function normalizeError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function sessionParticipants(session: WebRTCCallSession): CallParticipant[] {
  return (
    session.participants ?? [
      {
        peerId: session.peerId,
        status: session.status,
        remoteStream: session.remoteStream,
        lastError: session.lastError,
      },
    ]
  );
}

function participantPeerIds(call: Pick<IncomingCall, 'participants'>): string[] {
  return call.participants.map((participant) => participant.peerId);
}

function isGroupCall(call: Pick<IncomingCall, 'mode' | 'participants'>): boolean {
  return call.mode === 'group' || call.participants.length > 1;
}

function toIncomingCall(
  payload: Pick<CallOfferEventPayload, 'callerId'>,
  session: WebRTCCallSession,
  currentIncomingCall?: IncomingCall | null
): IncomingCall {
  return {
    callId: session.callId,
    groupId: session.groupId,
    peerId: session.peerId,
    callerId: currentIncomingCall?.callerId ?? payload.callerId,
    callType: session.callType,
    status: session.status,
    mode: session.mode,
    participants: sessionParticipants(session),
  };
}

function mediaStateFromSession(
  session: WebRTCCallSession
): Pick<CallStore, 'isMuted' | 'isCameraOff' | 'isScreenSharing'> {
  const audioTrack = session.localStream?.getAudioTracks()[0] ?? null;
  const videoTrack = session.localStream?.getVideoTracks()[0] ?? null;

  return {
    isMuted: audioTrack ? !audioTrack.enabled : false,
    isCameraOff: session.callType === 'audio' ? true : videoTrack ? !videoTrack.enabled : false,
    isScreenSharing: session.isScreenSharing ?? false,
  };
}

function withSessionState(
  state: Pick<CallStore, 'activeCall' | 'incomingCall' | 'isScreenSharing'>,
  session: WebRTCCallSession
) {
  if (session.status === 'closed') {
    return clearTrackedCall(state, session.callId);
  }

  return {
    activeCall:
      state.activeCall?.callId === session.callId ||
      (session.direction === 'outgoing' && !state.activeCall && !state.incomingCall)
        ? session
        : state.activeCall,
    incomingCall:
      state.incomingCall?.callId === session.callId
        ? toIncomingCall({ callerId: state.incomingCall.callerId }, session, state.incomingCall)
        : state.incomingCall,
  };
}

function shouldSyncMediaState(
  state: Pick<CallStore, 'activeCall' | 'incomingCall'>,
  session: WebRTCCallSession
): boolean {
  return (
    state.activeCall?.callId === session.callId ||
    (session.direction === 'outgoing' && !state.activeCall && !state.incomingCall)
  );
}

function reportCallBridgeError(error: unknown): void {
  console.error('Failed to process call signaling event:', normalizeError(error));
}

function runCallHandler(action: Promise<unknown>): void {
  void action.catch(reportCallBridgeError);
}

function isCapacityError(error: unknown): boolean {
  return normalizeError(error).message === GROUP_CALL_CAPACITY_ERROR;
}

export function createCallStore(
  dependencies: CallStoreDependencies = defaultDependencies
): CallStoreHook {
  let incomingCallTimeout: ReturnType<typeof setTimeout> | null = null;
  let incomingCallTimeoutCallId: string | null = null;

  return create<CallStore>((set, get) => ({
    ...(() => {
      const clearIncomingCallTimeout = (callId?: string): void => {
        if (callId && incomingCallTimeoutCallId && incomingCallTimeoutCallId !== callId) {
          return;
        }

        if (incomingCallTimeout) {
          clearTimeout(incomingCallTimeout);
          incomingCallTimeout = null;
        }

        incomingCallTimeoutCallId = null;
      };

      const autoDeclineIncomingCall = async (callId: string): Promise<void> => {
        const incomingCall = get().incomingCall;
        if (!incomingCall || incomingCall.callId !== callId) {
          clearIncomingCallTimeout(callId);
          return;
        }

        clearIncomingCallTimeout(callId);

        let rejectionError: unknown;
        for (const peerId of participantPeerIds(incomingCall)) {
          try {
            await dependencies.rejectCall(peerId, incomingCall.callId, 'no-answer');
          } catch (error) {
            rejectionError ??= error;
          }
        }

        await dependencies.service.handleRemoteHangup(incomingCall.callId);
        set((state) => clearTrackedCall(state, incomingCall.callId));

        if (rejectionError) {
          throw rejectionError;
        }
      };

      const scheduleIncomingCallTimeout = (callId: string): void => {
        if (incomingCallTimeout && incomingCallTimeoutCallId === callId) {
          return;
        }

        clearIncomingCallTimeout();
        incomingCallTimeoutCallId = callId;
        incomingCallTimeout = setTimeout(() => {
          void autoDeclineIncomingCall(callId).catch(reportCallBridgeError);
        }, CALL_NO_ANSWER_TIMEOUT_MS);
      };

      return {
        activeCall: null,
        incomingCall: null,
        devices: [],
        isMuted: false,
        isCameraOff: false,
        isScreenSharing: false,
        selectedAudioDeviceId: null,
        selectedVideoDeviceId: null,

        enumerateDevices: async () => {
          try {
            const devices = await dependencies.service.getMediaDevices();
            set({ devices });
          } catch (error) {
            console.error('Failed to enumerate devices', error);
          }
        },

        toggleMute: () => {
          const { activeCall, isMuted } = get();
          if (!activeCall) {
            return;
          }

          dependencies.service.toggleMute(activeCall.callId);
          set({ isMuted: !isMuted });
        },

        toggleCamera: () => {
          const { activeCall, isCameraOff } = get();
          if (!activeCall) {
            return;
          }

          dependencies.service.toggleCamera(activeCall.callId);
          set({ isCameraOff: !isCameraOff });
        },

        toggleScreenShare: async () => {
          const { activeCall, isScreenSharing } = get();
          if (!activeCall) {
            return;
          }

          const session = isScreenSharing
            ? await dependencies.service.stopScreenShare(activeCall.callId)
            : await dependencies.service.startScreenShare(activeCall.callId);

          set((state) => ({
            ...withSessionState(state, session),
            ...mediaStateFromSession(session),
          }));
        },

        setAudioDevice: async (deviceId: string) => {
          const { activeCall } = get();
          set({ selectedAudioDeviceId: deviceId });
          if (activeCall) {
            await dependencies.service.switchAudioDevice(activeCall.callId, deviceId);
          }
        },

        setVideoDevice: async (deviceId: string) => {
          const { activeCall } = get();
          set({ selectedVideoDeviceId: deviceId });
          if (activeCall) {
            await dependencies.service.switchVideoDevice(activeCall.callId, deviceId);
          }
        },

        startCall: async (peerId, callType) => {
          if (trackedCallId(get())) {
            throw new Error('A call is already in progress');
          }

          const session = await dependencies.service.startCall(peerId, callType);
          set({
            activeCall: session,
            incomingCall: null,
            ...mediaStateFromSession(session),
          });
          return session;
        },

        startGroupCall: async (groupId, peerIds, callType) => {
          if (trackedCallId(get())) {
            throw new Error('A call is already in progress');
          }

          const session = await dependencies.service.startGroupCall(groupId, peerIds, callType);
          set({
            activeCall: session,
            incomingCall: null,
            ...mediaStateFromSession(session),
          });
          return session;
        },

        joinGroupCall: async (callId, groupId, peerIds, callType) => {
          const currentTrackedCallId = trackedCallId(get());
          if (currentTrackedCallId && currentTrackedCallId !== callId) {
            throw new Error('A call is already in progress');
          }

          const session = await dependencies.service.joinGroupCall(
            callId,
            groupId,
            peerIds,
            callType
          );
          set({
            activeCall: session,
            incomingCall: null,
            ...mediaStateFromSession(session),
          });
          return session;
        },

        acceptIncomingCall: async () => {
          const incomingCall = get().incomingCall;
          if (!incomingCall) {
            return null;
          }

          clearIncomingCallTimeout(incomingCall.callId);

          const session = isGroupCall(incomingCall)
            ? await dependencies.service.joinGroupCall(
                incomingCall.callId,
                incomingCall.groupId,
                participantPeerIds(incomingCall),
                incomingCall.callType
              )
            : await dependencies.service.answerCall(incomingCall.callId, incomingCall.peerId);

          set({
            activeCall: session,
            incomingCall: null,
            ...mediaStateFromSession(session),
          });
          return session;
        },

        rejectIncomingCall: async (reason) => {
          const incomingCall = get().incomingCall;
          if (!incomingCall) {
            return;
          }

          clearIncomingCallTimeout(incomingCall.callId);

          let rejectionError: unknown;
          for (const peerId of participantPeerIds(incomingCall)) {
            try {
              await dependencies.rejectCall(peerId, incomingCall.callId, reason);
            } catch (error) {
              rejectionError ??= error;
            }
          }

          await dependencies.service.handleRemoteHangup(incomingCall.callId);
          set((state) => clearTrackedCall(state, incomingCall.callId));

          if (rejectionError) {
            throw rejectionError;
          }
        },

        hangupActiveCall: async () => {
          const activeCall = get().activeCall;
          if (!activeCall) {
            return;
          }

          let hangupError: unknown;
          try {
            if (activeCall.mode === 'group') {
              await dependencies.service.leaveGroupCall(activeCall.callId);
            } else {
              await dependencies.service.hangup(activeCall.callId);
            }
          } catch (error) {
            hangupError = error;
          }

          set((state) => clearTrackedCall(state, activeCall.callId));

          if (hangupError) {
            throw hangupError;
          }
        },

        leaveGroupCall: async () => {
          const activeCall = get().activeCall;
          if (!activeCall) {
            return;
          }

          let leaveError: unknown;
          try {
            await dependencies.service.leaveGroupCall(activeCall.callId);
          } catch (error) {
            leaveError = error;
          }

          set((state) => clearTrackedCall(state, activeCall.callId));

          if (leaveError) {
            throw leaveError;
          }
        },

        syncSession: (session) => {
          if (session.status === 'closed') {
            clearIncomingCallTimeout(session.callId);
          }

          set((state) => {
            const nextState = withSessionState(state, session);
            if (!shouldSyncMediaState(state, session)) {
              return nextState;
            }

            return {
              ...nextState,
              ...mediaStateFromSession(session),
            };
          });
        },

        handleCallOffer: async (payload) => {
          const state = get();
          const currentTrackedCallId = trackedCallId(state);
          if (currentTrackedCallId && currentTrackedCallId !== payload.callId) {
            await dependencies.rejectCall(payload.peerId, payload.callId, 'busy');
            return;
          }

          try {
            const session = await dependencies.service.handleOffer(
              payload.callId,
              payload.peerId,
              payload.sdp,
              payload.callType
            );

            set((currentState) => {
              const nextState = withSessionState(currentState, session);
              if (currentState.activeCall?.callId === payload.callId) {
                return nextState;
              }

              return {
                ...nextState,
                incomingCall: toIncomingCall(payload, session, currentState.incomingCall),
              };
            });

            const nextState = get();
            if (nextState.incomingCall?.callId === payload.callId && !nextState.activeCall) {
              scheduleIncomingCallTimeout(payload.callId);
            }
          } catch (error) {
            if (isCapacityError(error)) {
              await dependencies.rejectCall(payload.peerId, payload.callId, 'capacity');
              return;
            }

            throw error;
          }
        },

        handleCallJoin: async (payload) => {
          const state = get();
          const activeCall = state.activeCall?.callId === payload.callId ? state.activeCall : null;
          const incomingCall =
            state.incomingCall?.callId === payload.callId ? state.incomingCall : null;

          try {
            const session = await dependencies.service.handleCallJoin(
              payload.callId,
              payload.peerId,
              payload.groupId,
              !!activeCall,
              activeCall?.callType ?? incomingCall?.callType ?? 'video'
            );

            if (!activeCall && !incomingCall) {
              return;
            }

            set((currentState) => {
              const nextState = withSessionState(currentState, session);
              if (currentState.incomingCall?.callId === payload.callId) {
                return {
                  ...nextState,
                  incomingCall: {
                    ...toIncomingCall(
                      { callerId: currentState.incomingCall.callerId },
                      session,
                      currentState.incomingCall
                    ),
                    groupId: payload.groupId,
                  },
                };
              }

              return nextState;
            });
          } catch (error) {
            if (isCapacityError(error)) {
              await dependencies.rejectCall(payload.peerId, payload.callId, 'capacity');
              return;
            }

            throw error;
          }
        },

        handleCallAnswer: async (payload) => {
          if (!isTrackedCall(get(), payload.callId)) {
            return;
          }

          const session = await dependencies.service.handleAnswer(
            payload.callId,
            payload.sdp,
            payload.peerId
          );
          set((state) => withSessionState(state, session));
        },

        handleIceCandidate: async (payload) => {
          if (!isTrackedCall(get(), payload.callId)) {
            return;
          }

          const session = await dependencies.service.handleIceCandidate(
            payload.callId,
            {
              candidate: payload.candidate,
              sdpMid: payload.sdpMid,
              sdpMlineIndex: payload.sdpMlineIndex,
            },
            payload.peerId
          );
          set((state) => withSessionState(state, session));
        },

        handleCallHangup: async (payload) => {
          if (!isTrackedCall(get(), payload.callId)) {
            return;
          }

          await dependencies.service.handleRemoteHangup(payload.callId, payload.peerId);
        },

        handleCallReject: async (payload) => {
          if (!isTrackedCall(get(), payload.callId)) {
            return;
          }

          await dependencies.service.handleRemoteHangup(payload.callId, payload.peerId);
        },

        handleCallLeave: async (payload) => {
          await dependencies.service.handleRemoteHangup(payload.callId, payload.peerId);
        },
      };
    })(),
  }));
}

export const useCallStore = createCallStore();

export async function setupCallEventListeners(
  store: CallStoreHook = useCallStore,
  dependencies: CallStoreDependencies = defaultDependencies
): Promise<() => void> {
  const unlistenService = dependencies.service.subscribe((session) => {
    store.getState().syncSession(session);
  });

  const unlistenOffer = await dependencies.listen<CallOfferEventPayload>('call-offer', (event) => {
    runCallHandler(store.getState().handleCallOffer(event.payload));
  });

  const unlistenJoin = await dependencies.listen<CallJoinEventPayload>('call-join', (event) => {
    runCallHandler(store.getState().handleCallJoin(event.payload));
  });

  const unlistenAnswer = await dependencies.listen<CallAnswerEventPayload>(
    'call-answer',
    (event) => {
      runCallHandler(store.getState().handleCallAnswer(event.payload));
    }
  );

  const unlistenIceCandidate = await dependencies.listen<IceCandidateEventPayload>(
    'ice-candidate',
    (event) => {
      runCallHandler(store.getState().handleIceCandidate(event.payload));
    }
  );

  const unlistenHangup = await dependencies.listen<CallHangupEventPayload>(
    'call-hangup',
    (event) => {
      runCallHandler(store.getState().handleCallHangup(event.payload));
    }
  );

  const unlistenReject = await dependencies.listen<CallRejectEventPayload>(
    'call-reject',
    (event) => {
      runCallHandler(store.getState().handleCallReject(event.payload));
    }
  );

  const unlistenLeave = await dependencies.listen<CallLeaveEventPayload>('call-leave', (event) => {
    runCallHandler(store.getState().handleCallLeave(event.payload));
  });

  return () => {
    unlistenService();
    unlistenOffer();
    unlistenJoin();
    unlistenAnswer();
    unlistenIceCandidate();
    unlistenHangup();
    unlistenReject();
    unlistenLeave();
  };
}
