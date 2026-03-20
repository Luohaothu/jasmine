import React, { useEffect, useRef } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { useChatStore, ChatMessage } from "../../stores/chatStore";
import { usePeerStore } from "../../stores/peerStore";
import { ChatHeader } from "./ChatHeader";
import { MessageBubble } from "./MessageBubble";
import styles from "./ChatView.module.css";

const EMPTY_ARRAY: ChatMessage[] = [];

export const ChatView: React.FC = () => {
  const { peerId } = useParams<{ peerId: string }>();
  const navigate = useNavigate();
  
  const messages = useChatStore((state) => (peerId ? state.messages[peerId] || EMPTY_ARRAY : EMPTY_ARRAY));
  const addMessage = useChatStore((state) => state.addMessage);
  
  const peer = usePeerStore((state) =>
    state.peers.find((p) => p.id === peerId)
  );
  
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const scrollToBottom = () => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  };

  useEffect(() => {
    scrollToBottom();
  }, [messages]);

  useEffect(() => {
    if (!peerId) return;

    const setupListener = async () => {
      const unlisten = await listen<ChatMessage>(
        "message-received",
        (event) => {
          const msg = event.payload;
          if (msg.senderId === peerId || msg.receiverId === peerId) {
            addMessage(peerId, msg);
          }
        }
      );
      return unlisten;
    };

    const promise = setupListener();

    return () => {
      promise.then((unlisten) => unlisten()).catch((error) => {
        void error;
      });
    };
  }, [peerId, addMessage]);

  const handleBack = () => {
    navigate("/");
  };

  if (!peerId) return null;

  return (
    <div className={styles.container}>
      <ChatHeader
        peerName={peer?.name || "Unknown"}
        status={peer?.status || "offline"}
        onBack={handleBack}
      />
      <div className={styles.messageList}>
        {messages.length === 0 ? (
          <div className={styles.emptyState}>发送第一条消息开始聊天</div>
        ) : (
          messages.map((msg) => (
            <MessageBubble
              key={msg.id}
              content={msg.content}
              timestamp={msg.timestamp}
              isOwn={msg.senderId === "local"}
              status={msg.status}
            />
          ))
        )}
        <div ref={messagesEndRef} />
      </div>
    </div>
  );
};
