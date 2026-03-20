import React from "react";
import styles from "./MessageBubble.module.css";

export interface MessageBubbleProps {
  content: string;
  timestamp: number;
  isOwn: boolean;
  status?: "sent" | "delivered" | "failed";
}

export const MessageBubble: React.FC<MessageBubbleProps> = ({
  content,
  timestamp,
  isOwn,
  status,
}) => {
  const date = new Date(timestamp);
  const hours = date.getHours().toString().padStart(2, "0");
  const minutes = date.getMinutes().toString().padStart(2, "0");
  const timeString = `${hours}:${minutes}`;

  const renderStatus = () => {
    if (!isOwn || !status) return null;
    switch (status) {
      case "sent":
        return <span className={styles.status}>✓</span>;
      case "delivered":
        return <span className={styles.status}>✓✓</span>;
      case "failed":
        return <span className={styles.status}>❌</span>;
      default:
        return null;
    }
  };

  return (
    <div className={`${styles.container} ${isOwn ? styles.own : styles.peer}`}>
      <div className={`${styles.bubble} ${isOwn ? styles.ownBubble : styles.peerBubble}`}>
        <div className={styles.content}>{content}</div>
        <div className={`${styles.meta} ${isOwn ? styles.ownMeta : styles.peerMeta}`}>
          <span className={styles.time}>{timeString}</span>
          {renderStatus()}
        </div>
      </div>
    </div>
  );
};
