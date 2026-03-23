import { listen } from '@tauri-apps/api/event';
import { create } from 'zustand';

export interface ChatMessage {
  id: string;
  senderId: string;
  receiverId: string;
  content: string;
  timestamp: number;
  encrypted?: boolean;
  status: 'sent' | 'delivered' | 'failed';
  editVersion?: number;
  isDeleted?: boolean;
  editedAt?: number;
  deletedAt?: number;
  replyToId?: string;
  replyToPreview?: string;
  type?: 'text' | 'image' | 'file';
  transferId?: string;
  metadata?: {
    fileName?: string;
    size?: number;
    originalPath?: string;
    thumbnailPath?: string;
    thumbnailState?: 'pending' | 'ready' | 'failed';
  };
}

export interface ReplyingTo {
  id: string;
  preview: string;
  senderName?: string;
}

interface MessageEditedEventPayload {
  messageId: string;
  newContent: string;
  editVersion: number;
  editedAt: number;
}

interface MessageDeletedEventPayload {
  messageId: string;
}

interface ThumbnailReadyEventPayload {
  transferId: string;
  thumbnailPath: string;
}

interface ThumbnailFailedEventPayload {
  transferId: string;
}

interface MentionReceivedEventPayload {
  messageId: string;
  mentionedUserId: string;
  senderName: string;
}

/* eslint-disable no-unused-vars */
interface ChatStore {
  messages: Record<string, ChatMessage[]>;
  replyingTo: ReplyingTo | null;
  addMessage: (peerId: string, message: ChatMessage) => void;
  setMessages: (peerId: string, messages: ChatMessage[]) => void;
  editMessage: (peerId: string, messageId: string, newContent: string) => void;
  deleteMessage: (peerId: string, messageId: string) => void;
  applyMessageEdited: (
    messageId: string,
    newContent: string,
    editedAt: number,
    editVersion: number
  ) => void;
  applyMessageDeleted: (messageId: string) => void;
  applyThumbnailReady: (transferId: string, thumbnailPath: string) => void;
  applyThumbnailFailed: (transferId: string) => void;
  setReplyingTo: (reply: ReplyingTo | null) => void;
}
/* eslint-enable no-unused-vars */

export const useChatStore = create<ChatStore>((set) => ({
  messages: {},
  replyingTo: null,
  addMessage: (peerId, message) =>
    set((state) => ({
      messages: {
        ...state.messages,
        [peerId]: [...(state.messages[peerId] || []), message],
      },
    })),
  setMessages: (peerId, messages) =>
    set((state) => ({
      messages: {
        ...state.messages,
        [peerId]: messages,
      },
    })),
  editMessage: (peerId, messageId, newContent) =>
    set((state) => {
      const peerMessages = state.messages[peerId] || [];
      return {
        messages: {
          ...state.messages,
          [peerId]: peerMessages.map((msg) =>
            msg.id === messageId ? { ...msg, content: newContent, editedAt: Date.now() } : msg
          ),
        },
      };
    }),
  deleteMessage: (peerId, messageId) =>
    set((state) => {
      const peerMessages = state.messages[peerId] || [];
      return {
        messages: {
          ...state.messages,
          [peerId]: peerMessages.map((msg) =>
            msg.id === messageId
              ? { ...msg, isDeleted: true, content: '', deletedAt: Date.now() }
              : msg
          ),
        },
      };
    }),
  applyMessageEdited: (messageId, newContent, editedAt, editVersion) =>
    set((state) => ({
      messages: Object.fromEntries(
        Object.entries(state.messages).map(([peerId, peerMessages]) => [
          peerId,
          peerMessages.map((msg) =>
            msg.id === messageId && !msg.isDeleted && editVersion > (msg.editVersion || 0)
              ? { ...msg, content: newContent, editedAt, editVersion }
              : msg
          ),
        ])
      ),
    })),
  applyMessageDeleted: (messageId) =>
    set((state) => ({
      messages: Object.fromEntries(
        Object.entries(state.messages).map(([peerId, peerMessages]) => [
          peerId,
          peerMessages.map((msg) =>
            msg.id === messageId
              ? { ...msg, isDeleted: true, content: '', deletedAt: msg.deletedAt ?? Date.now() }
              : msg
          ),
        ])
      ),
    })),
  applyThumbnailReady: (transferId, thumbnailPath) =>
    set((state) => ({
      messages: Object.fromEntries(
        Object.entries(state.messages).map(([peerId, peerMessages]) => [
          peerId,
          peerMessages.map((msg) =>
            msg.transferId === transferId
              ? {
                  ...msg,
                  type: msg.type ?? 'image',
                  metadata: {
                    ...msg.metadata,
                    thumbnailPath,
                    thumbnailState: 'ready',
                  },
                }
              : msg
          ),
        ])
      ),
    })),
  applyThumbnailFailed: (transferId) =>
    set((state) => ({
      messages: Object.fromEntries(
        Object.entries(state.messages).map(([peerId, peerMessages]) => [
          peerId,
          peerMessages.map((msg) =>
            msg.transferId === transferId
              ? {
                  ...msg,
                  type: msg.type ?? 'image',
                  metadata: {
                    ...msg.metadata,
                    thumbnailState: 'failed',
                  },
                }
              : msg
          ),
        ])
      ),
    })),
  setReplyingTo: (reply) => set({ replyingTo: reply }),
}));

export async function setupChatEventListeners(): Promise<() => void> {
  const unlistenEdited = await listen<MessageEditedEventPayload>('message-edited', (event) => {
    useChatStore
      .getState()
      .applyMessageEdited(
        event.payload.messageId,
        event.payload.newContent,
        event.payload.editedAt,
        event.payload.editVersion
      );
  });

  const unlistenDeleted = await listen<MessageDeletedEventPayload>('message-deleted', (event) => {
    useChatStore.getState().applyMessageDeleted(event.payload.messageId);
  });

  const unlistenThumbnailReady = await listen<ThumbnailReadyEventPayload>(
    'thumbnail-ready',
    (event) => {
      useChatStore
        .getState()
        .applyThumbnailReady(event.payload.transferId, event.payload.thumbnailPath);
    }
  );

  const unlistenThumbnailFailed = await listen<ThumbnailFailedEventPayload>(
    'thumbnail-failed',
    (event) => {
      useChatStore.getState().applyThumbnailFailed(event.payload.transferId);
    }
  );

  const unlistenMentionReceived = await listen<MentionReceivedEventPayload>(
    'mention-received',
    (event) => {
      window.dispatchEvent(
        new CustomEvent('jasmine:mention-received', {
          detail: event.payload,
        })
      );
    }
  );

  return () => {
    unlistenEdited();
    unlistenDeleted();
    unlistenThumbnailReady();
    unlistenThumbnailFailed();
    unlistenMentionReceived();
  };
}
