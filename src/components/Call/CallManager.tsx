import React, { useEffect } from 'react';
import { useCallStore, setupCallEventListeners } from '../../stores/callStore';
import { IncomingCallOverlay } from './IncomingCallOverlay';
import { OutgoingCallOverlay } from './OutgoingCallOverlay';
import { InCallView } from './InCallView';

export const CallManager: React.FC = () => {
  const activeCall = useCallStore((state) => state.activeCall);
  const incomingCall = useCallStore((state) => state.incomingCall);
  const acceptIncomingCall = useCallStore((state) => state.acceptIncomingCall);
  const rejectIncomingCall = useCallStore((state) => state.rejectIncomingCall);
  const hangupActiveCall = useCallStore((state) => state.hangupActiveCall);

  useEffect(() => {
    const setup = async () => {
      try {
        const cleanup = await setupCallEventListeners();
        return cleanup;
      } catch (error) {
        console.error('Failed to setup call event listeners:', error);
      }
    };

    let cleanupFn: (() => void) | undefined;
    setup().then((fn) => {
      cleanupFn = fn;
    });

    return () => {
      if (cleanupFn) cleanupFn();
    };
  }, []);

  if (activeCall) {
    if (activeCall.status === 'new' || activeCall.status === 'connecting') {
      return (
        <OutgoingCallOverlay
          peerId={activeCall.peerId}
          status={activeCall.status as 'new' | 'connecting'}
          onEnd={hangupActiveCall}
        />
      );
    }

    if (activeCall.status === 'connected' || activeCall.status === 'disconnected') {
      return <InCallView session={activeCall} onEnd={hangupActiveCall} />;
    }
  }

  if (incomingCall && incomingCall.status === 'new') {
    return (
      <IncomingCallOverlay
        peerId={incomingCall.peerId}
        onAccept={acceptIncomingCall}
        onReject={() => rejectIncomingCall()}
      />
    );
  }

  return null;
};
