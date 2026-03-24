import React, { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useParams, useNavigate } from 'react-router-dom';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import { useChatStore, ChatMessage } from '../../stores/chatStore';
import { usePeerStore } from '../../stores/peerStore';
import { useCallStore } from '../../stores/callStore';
import { ChatHeader } from './ChatHeader';
import { MessageBubble } from './MessageBubble';
import { ReplyThread } from './ReplyThread';
import { MessageInput } from '../MessageInput/MessageInput';
import styles from './ChatView.module.css';

const EMPTY_ARRAY: ChatMessage[] = [];

type MessageGroup =
  | { type: 'single'; message: ChatMessage }
  | { type: 'thread'; parentId: string; parentPreview: string; messages: ChatMessage[] };

type ReplyCountMap = Record<string, number>;

export const ChatView: React.FC = () => {
  const { peerId } = useParams<{ peerId: string }>();
  const navigate = useNavigate();
  const { t } = useTranslation();

  const messages = useChatStore((state) =>
    peerId ? state.messages[peerId] || EMPTY_ARRAY : EMPTY_ARRAY
  );
  const addMessage = useChatStore((state) => state.addMessage);
  const [replyCounts, setReplyCounts] = useState<ReplyCountMap>({});

  const peer = usePeerStore((state) => state.peers.find((p) => p.id === peerId));
  const startCall = useCallStore((state) => state.startCall);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const onlineStatus = t('chat.status.online') as 'online';
  const offlineStatus = t('chat.status.offline') as 'offline';
  const messageIds = useMemo(() => messages.map((message) => message.id), [messages]);
  const messagesWithReplyCounts = useMemo(
    () =>
      messages.map((message) => ({
        ...message,
        replyCount: replyCounts[message.id] ?? 0,
      })),
    [messages, replyCounts]
  );
  const firstReplyIds = useMemo(() => {
    const nextFirstReplyIds: Record<string, string> = {};

    for (const message of messagesWithReplyCounts) {
      if (message.replyToId && !nextFirstReplyIds[message.replyToId]) {
        nextFirstReplyIds[message.replyToId] = message.id;
      }
    }

    return nextFirstReplyIds;
  }, [messagesWithReplyCounts]);

  useEffect(() => {
    if (messageIds.length === 0) {
      return;
    }

    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messageIds]);

  useEffect(() => {
    let isCancelled = false;

    if (messageIds.length === 0) {
      setReplyCounts({});
      return () => {
        isCancelled = true;
      };
    }

    void invoke<ReplyCountMap>('get_reply_counts', {
      messageIds,
    })
      .then((counts) => {
        if (!isCancelled) {
          setReplyCounts(counts ?? {});
        }
      })
      .catch((error) => {
        if (!isCancelled) {
          setReplyCounts({});
        }
        console.error('Failed to load reply counts:', error);
      });

    return () => {
      isCancelled = true;
    };
  }, [messageIds]);

  useEffect(() => {
    if (!peerId) return;

    const setupListener = async () => {
      const unlisten = await listen<ChatMessage>('message-received', (event) => {
        const msg = event.payload;
        if (msg.senderId === peerId || msg.receiverId === peerId) {
          addMessage(peerId, msg);
        }
      });
      return unlisten;
    };

    const promise = setupListener();

    return () => {
      promise
        .then((unlisten) => unlisten())
        .catch((error) => {
          console.error('Failed to unlisten message-received:', error);
        });
    };
  }, [peerId, addMessage]);

  const handleBack = () => {
    navigate('/');
  };

  const handleAudioCall = async () => {
    if (peerId) {
      await startCall(peerId, 'audio').catch((err) => {
        console.error('Failed to start audio call', err);
      });
    }
  };

  const handleVideoCall = async () => {
    if (peerId) {
      await startCall(peerId, 'video').catch((err) => {
        console.error('Failed to start video call', err);
      });
    }
  };

  if (!peerId) return null;

  const groupedMessages: MessageGroup[] = [];
  let currentThread: { parentId: string; parentPreview: string; messages: ChatMessage[] } | null =
    null;

  for (const msg of messagesWithReplyCounts) {
    if (msg.replyToId) {
      if (currentThread && currentThread.parentId === msg.replyToId) {
        currentThread.messages.push(msg);
        if (msg.replyToPreview) {
          currentThread.parentPreview = msg.replyToPreview;
        }
      } else {
        if (currentThread) {
          if (currentThread.messages.length === 1) {
            groupedMessages.push({ type: 'single', message: currentThread.messages[0] });
          } else {
            groupedMessages.push({
              type: 'thread',
              parentId: currentThread.parentId,
              parentPreview: currentThread.parentPreview,
              messages: currentThread.messages,
            });
          }
        }
        currentThread = {
          parentId: msg.replyToId,
          parentPreview: msg.replyToPreview || '',
          messages: [msg],
        };
      }
    } else {
      if (currentThread) {
        if (currentThread.messages.length === 1) {
          groupedMessages.push({ type: 'single', message: currentThread.messages[0] });
        } else {
          groupedMessages.push({
            type: 'thread',
            parentId: currentThread.parentId,
            parentPreview: currentThread.parentPreview,
            messages: currentThread.messages,
          });
        }
        currentThread = null;
      }
      groupedMessages.push({ type: 'single', message: msg });
    }
  }

  if (currentThread) {
    if (currentThread.messages.length === 1) {
      groupedMessages.push({ type: 'single', message: currentThread.messages[0] });
    } else {
      groupedMessages.push({
        type: 'thread',
        parentId: currentThread.parentId,
        parentPreview: currentThread.parentPreview,
        messages: currentThread.messages,
      });
    }
  }

  const renderMessage = (msg: ChatMessage, isThreaded: boolean) => (
    <MessageBubble
      key={msg.id}
      id={msg.id}
      content={msg.content}
      timestamp={msg.timestamp}
      isOwn={msg.senderId === 'local'}
      senderName={msg.senderId === 'local' ? t('chat.you') : peer?.name}
      status={msg.status}
      isDeleted={msg.isDeleted}
      editedAt={msg.editedAt}
      replyToId={msg.replyToId}
      replyToPreview={msg.replyToPreview}
      replyCount={msg.replyCount}
      firstReplyId={firstReplyIds[msg.id]}
      hideQuote={isThreaded}
      type={msg.type}
      metadata={msg.metadata}
      onEdit={(id, newContent) => {
        invoke('edit_message', { messageId: id, newContent }).catch((error) => {
          console.error('Failed to edit message:', error);
        });
      }}
      onDelete={(id) => {
        invoke('delete_message', { messageId: id }).catch((error) => {
          console.error('Failed to delete message:', error);
        });
      }}
      onReply={(id, preview, senderName) =>
        useChatStore.getState().setReplyingTo({ id, preview, senderName })
      }
    />
  );

  return (
    <div className={styles.container}>
      <ChatHeader
        peerName={peer?.name || t('chat.unknownPeer')}
        status={peer?.status === 'online' ? onlineStatus : offlineStatus}
        onBack={handleBack}
        onAudioCall={handleAudioCall}
        onVideoCall={handleVideoCall}
      />
      <div className={styles.messageList}>
        {groupedMessages.length === 0 ? (
          <div className={styles.emptyState}>{t('chat.empty')}</div>
        ) : (
          groupedMessages.map((group) => {
            if (group.type === 'single') {
              return renderMessage(group.message, false);
            }
            return (
              <ReplyThread
                key={`thread-${group.parentId}-${group.messages[0].id}`}
                replyToId={group.parentId}
                replyToPreview={group.parentPreview}
              >
                {group.messages.map((msg) => renderMessage(msg, true))}
              </ReplyThread>
            );
          })
        )}
        <div ref={messagesEndRef} />
      </div>
      <MessageInput peerId={peerId} />
    </div>
  );
};
