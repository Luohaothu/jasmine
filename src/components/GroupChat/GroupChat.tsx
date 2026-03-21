import React, { useState } from "react";
import styles from "./GroupChat.module.css";
import { MemberPanel } from "./MemberPanel";
import { Peer } from "../../types/peer";
import { MessageBubble } from "../ChatView/MessageBubble";
import { MessageInput } from "../MessageInput/MessageInput";
import { useChatStore } from "../../stores/chatStore";

export interface GroupMessage {
  id: string;
  senderId: string;
  senderName?: string;
  content: string;
  timestamp: number;
  isOwn: boolean;
  status?: "sent" | "delivered" | "failed";
  isDeleted?: boolean;
  editedAt?: number;
  replyToId?: string;
  replyToPreview?: string;
}

export interface GroupChatProps {
  groupName: string;
  members: Peer[];
  messages: GroupMessage[];
  // eslint-disable-next-line no-unused-vars
  onSendMessage: (content: string, replyToId?: string) => void;
}

export const GroupChat: React.FC<GroupChatProps> = ({
  groupName,
  members,
  messages,
  onSendMessage,
}) => {
  const [isPanelOpen, setIsPanelOpen] = useState(false);
  const setReplyingTo = useChatStore((state) => state.setReplyingTo);

  return (
    <div className={styles.container}>
      <div className={styles.main}>
        <div className={styles.header}>
          <div className={styles.headerInfo}>
            <h2 className={styles.title}>{groupName}</h2>
            <p className={styles.subtitle}>{members.length} members</p>
          </div>
          <button
            className={`${styles.manageBtn} ${isPanelOpen ? styles.activeManageBtn : ""}`}
            onClick={() => setIsPanelOpen(!isPanelOpen)}
          >
            管理 / Manage
          </button>
        </div>

        <div className={styles.messageList}>
          {messages.length === 0 ? (
            <div className={styles.emptyState}>No messages yet. Start the conversation!</div>
          ) : (
            messages.map((msg) => (
              <div
                key={msg.id}
                className={`${styles.messageWrapper} ${
                  msg.isOwn ? styles.ownWrapper : styles.peerWrapper
                }`}
              >
                {!msg.isOwn && msg.senderName && (
                  <span className={styles.senderName}>{msg.senderName}</span>
                )}
                {/* Reusing MessageBubble but wrapping it to handle senderName above it */}
                <MessageBubble
                  id={msg.id}
                  content={msg.content}
                  timestamp={msg.timestamp}
                  isOwn={msg.isOwn}
                  status={msg.status}
                  isDeleted={msg.isDeleted}
                  editedAt={msg.editedAt}
                  replyToId={msg.replyToId}
                  replyToPreview={msg.replyToPreview}
                  senderName={msg.senderName}
                  onReply={(id, preview, senderName) => setReplyingTo({ id, preview, senderName })}
                />
              </div>
            ))
          )}
        </div>
        
        <MessageInput onSend={onSendMessage} disabled={false} />
      </div>

      <MemberPanel isOpen={isPanelOpen} members={members} />
    </div>
  );
};
