import React, { useState } from "react";
import { RichTextRenderer } from "../RichTextRenderer";
import { ImageThumbnail } from "../ImageThumbnail/ImageThumbnail";
import styles from "./MessageBubble.module.css";

export interface MessageBubbleProps {
  id: string;
  content: string;
  timestamp: number;
  isOwn: boolean;
  senderName?: string;
  status?: "sent" | "delivered" | "failed";
  isDeleted?: boolean;
  editedAt?: number;
  replyToId?: string;
  replyToPreview?: string;
  type?: "text" | "image" | "file";
  metadata?: {
    fileName?: string;
    size?: number;
    originalPath?: string;
    thumbnailPath?: string;
    thumbnailState?: "pending" | "ready" | "failed";
  };
  // eslint-disable-next-line no-unused-vars
  onEdit?: (id: string, newContent: string) => void;
  // eslint-disable-next-line no-unused-vars
  onDelete?: (id: string) => void;
  // eslint-disable-next-line no-unused-vars
  onReply?: (id: string, preview: string, senderName?: string) => void;
}

export const MessageBubble: React.FC<MessageBubbleProps> = ({
  id,
  content,
  timestamp,
  isOwn,
  senderName,
  status,
  isDeleted,
  editedAt,
  replyToId,
  replyToPreview,
  type,
  metadata,
  onEdit,
  onDelete,
  onReply,
}) => {
  const [isEditing, setIsEditing] = useState(false);
  const [editContent, setEditContent] = useState(content);

  const date = new Date(timestamp);
  const hours = date.getHours().toString().padStart(2, "0");
  const minutes = date.getMinutes().toString().padStart(2, "0");
  const timeString = `${hours}:${minutes}`;

  const renderStatus = () => {
    if (!isOwn || !status || isDeleted) return null;
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

  const handleSave = () => {
    if (editContent.trim() && editContent !== content) {
      onEdit?.(id, editContent);
    }
    setIsEditing(false);
  };

  const handleCancel = () => {
    setEditContent(content);
    setIsEditing(false);
  };

  const handleDelete = () => {
    if (window.confirm("Are you sure you want to delete this message?")) {
      onDelete?.(id);
    }
  };

  const handleReply = () => {
    const preview = content.length > 80 ? content.slice(0, 80) + '…' : content;
    onReply?.(id, preview, senderName);
  };

  const handleQuoteClick = () => {
    if (replyToId) {
      const element = document.getElementById(`message-${replyToId}`);
      if (element) {
        element.scrollIntoView({ behavior: 'smooth', block: 'center' });
        element.classList.add(styles.highlight);
        setTimeout(() => {
          element.classList.remove(styles.highlight);
        }, 1500);
      }
    }
  };

  if (isDeleted) {
    return (
      <div id={`message-${id}`} className={`${styles.container} ${isOwn ? styles.own : styles.peer}`}>
        <div className={`${styles.bubble} ${styles.tombstone}`}>
          <div className={styles.content}>This message was deleted</div>
        </div>
      </div>
    );
  }

  return (
    <div id={`message-${id}`} className={`${styles.container} ${isOwn ? styles.own : styles.peer}`}>
      <div className={`${styles.bubble} ${isOwn ? styles.ownBubble : styles.peerBubble}`}>
        {replyToId && replyToPreview && (
          <div 
            className={styles.quoteBubble} 
            data-testid="quote-bubble"
            onClick={handleQuoteClick}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                handleQuoteClick();
              }
            }}
            role="button"
            tabIndex={0}
            aria-label={`Jump to replied message: ${replyToPreview}`}
          >
            <span className={styles.quotePreview}>{replyToPreview}</span>
          </div>
        )}
        {isEditing ? (
          <div className={styles.editContainer}>
            <textarea
              className={styles.editInput}
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  handleSave();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  handleCancel();
                }
              }}
              autoFocus
            />
            <div className={styles.editActions}>
              <button className={styles.saveButton} onClick={handleSave}>Save</button>
              <button className={styles.cancelButton} onClick={handleCancel}>Cancel</button>
            </div>
          </div>
        ) : (
          <div className={styles.content}>
            {type === "image" ? (
              <ImageThumbnail
                filePath={metadata?.originalPath}
                thumbnailPath={metadata?.thumbnailPath}
                thumbnailState={metadata?.thumbnailState || "pending"}
                fileName={metadata?.fileName || content}
              />
            ) : (
              <RichTextRenderer content={content} />
            )}
          </div>
        )}
        
        <div className={`${styles.meta} ${isOwn ? styles.ownMeta : styles.peerMeta}`}>
          {editedAt && !isEditing && <span className={styles.editedMark}>(edited)</span>}
          <span className={styles.time}>{timeString}</span>
          {renderStatus()}
        </div>

        {!isEditing && (
          <div className={styles.actions} data-testid="message-actions">
            <button className={styles.actionButton} onClick={handleReply} data-testid="action-reply">Reply</button>
            {isOwn && onEdit && (
              <button className={styles.actionButton} onClick={() => setIsEditing(true)} data-testid="action-edit">Edit</button>
            )}
            {isOwn && onDelete && (
              <button className={styles.actionButton} onClick={handleDelete} data-testid="action-delete">Delete</button>
            )}
          </div>
        )}
      </div>
    </div>
  );
};
