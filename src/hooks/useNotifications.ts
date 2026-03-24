import { useEffect, useRef } from 'react';
import type { MutableRefObject } from 'react';

import { listen } from '@tauri-apps/api/event';
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';

import type { ChatMessage } from '../stores/chatStore';
import { usePeerStore } from '../stores/peerStore';

interface MentionReceivedDetail {
  messageId: string;
  mentionedUserId: string;
  senderName: string;
}

type NotificationPermissionState = 'unknown' | 'granted' | 'denied' | 'unsupported';

const LOCAL_USER_ID = 'local';
const MAX_TRACKED_MESSAGES = 200;
const PROCESSED_MESSAGE_TTL_MS = 60_000;

function pruneProcessedMessages(processedMessages: Map<string, number>): void {
  const cutoff = Date.now() - PROCESSED_MESSAGE_TTL_MS;

  for (const [messageId, timestamp] of processedMessages) {
    if (timestamp < cutoff) {
      processedMessages.delete(messageId);
    }
  }

  while (processedMessages.size > MAX_TRACKED_MESSAGES) {
    const oldestMessageId = processedMessages.keys().next().value;
    if (!oldestMessageId) {
      break;
    }

    processedMessages.delete(oldestMessageId);
  }
}

function alreadyProcessed(messageId: string, processedMessages: Map<string, number>): boolean {
  pruneProcessedMessages(processedMessages);
  return processedMessages.has(messageId);
}

function markProcessed(messageId: string, processedMessages: Map<string, number>): void {
  processedMessages.set(messageId, Date.now());
  pruneProcessedMessages(processedMessages);
}

function resolveDirectMessageBody(message: ChatMessage): string {
  const content = message.content.trim();
  if (content) {
    return content;
  }

  if (message.type === 'image') {
    return 'Sent you an image';
  }

  if (message.type === 'file') {
    return 'Sent you a file';
  }

  return 'Sent you a message';
}

function resolveDirectMessageSenderName(message: ChatMessage): string {
  const peer = usePeerStore.getState().peers.find((candidate) => candidate.id === message.senderId);
  return peer?.name ?? message.senderId;
}

async function ensureNotificationPermission(
  permissionStateRef: MutableRefObject<NotificationPermissionState>
): Promise<boolean> {
  if (permissionStateRef.current === 'granted') {
    return true;
  }

  if (permissionStateRef.current === 'denied' || permissionStateRef.current === 'unsupported') {
    return false;
  }

  try {
    const permissionGranted = await isPermissionGranted();
    if (permissionGranted) {
      permissionStateRef.current = 'granted';
      return true;
    }

    const permission = await requestPermission();
    permissionStateRef.current = permission === 'granted' ? 'granted' : 'denied';
    return permissionStateRef.current === 'granted';
  } catch (error) {
    permissionStateRef.current = 'unsupported';
    console.error('Notification permission check failed:', error);
    return false;
  }
}

export function useNotifications(): void {
  const isFocusedRef = useRef(typeof document === 'undefined' ? true : document.hasFocus());
  const permissionStateRef = useRef<NotificationPermissionState>('unknown');
  const processedMessagesRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    let disposed = false;

    const syncFocusState = (): void => {
      isFocusedRef.current = document.hasFocus();
    };

    const notify = async (messageId: string, title: string, body: string): Promise<void> => {
      if (alreadyProcessed(messageId, processedMessagesRef.current)) {
        return;
      }

      markProcessed(messageId, processedMessagesRef.current);

      if (isFocusedRef.current) {
        return;
      }

      const permissionGranted = await ensureNotificationPermission(permissionStateRef);
      if (!permissionGranted || disposed) {
        return;
      }

      try {
        await sendNotification({ title, body });
      } catch (error) {
        console.error('Failed to send notification:', error);
      }
    };

    const handleDirectMessage = (message: ChatMessage): void => {
      if (message.senderId === LOCAL_USER_ID || message.receiverId !== LOCAL_USER_ID) {
        return;
      }

      void notify(
        message.id,
        resolveDirectMessageSenderName(message),
        resolveDirectMessageBody(message)
      );
    };

    const handleMentionReceived = (event: Event): void => {
      const detail = (event as CustomEvent<MentionReceivedDetail>).detail;
      if (!detail?.messageId) {
        return;
      }

      const senderName = detail.senderName?.trim() || 'Someone';
      void notify(
        detail.messageId,
        `${senderName} mentioned you`,
        'Open Jasmine to view the message.'
      );
    };

    syncFocusState();
    window.addEventListener('focus', syncFocusState);
    window.addEventListener('blur', syncFocusState);
    window.addEventListener('jasmine:mention-received', handleMentionReceived);

    const unlistenPromise = listen<ChatMessage>('message-received', (event) => {
      handleDirectMessage(event.payload);
    });

    unlistenPromise.catch((error) => {
      console.error('Failed to listen for notification events:', error);
    });

    return () => {
      disposed = true;
      window.removeEventListener('focus', syncFocusState);
      window.removeEventListener('blur', syncFocusState);
      window.removeEventListener('jasmine:mention-received', handleMentionReceived);

      unlistenPromise
        .then((unlisten) => {
          unlisten();
        })
        .catch((error) => {
          console.error('Failed to unlisten notification events:', error);
        });
    };
  }, []);
}
