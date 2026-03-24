import React, { useState, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { RichTextRenderer } from '../RichTextRenderer';
import { ImageThumbnail } from '../ImageThumbnail/ImageThumbnail';
import { OgPreviewCard } from './OgPreviewCard';
import { extractUrls } from '../../utils/url-detect';
import styles from './MessageBubble.module.css';

export interface MessageBubbleProps {
  id: string;
  content: string;
  timestamp: number;
  isOwn: boolean;
  encrypted?: boolean;
  senderName?: string;
  status?: 'sent' | 'delivered' | 'failed';
  isDeleted?: boolean;
  editedAt?: number;
  replyToId?: string;
  replyToPreview?: string;
  replyCount?: number;
  firstReplyId?: string;
  type?: 'text' | 'image' | 'file';
  metadata?: {
    fileName?: string;
    size?: number;
    originalPath?: string;
    thumbnailPath?: string;
    thumbnailState?: 'pending' | 'ready' | 'failed';
  };
  hideQuote?: boolean;
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
  encrypted,
  senderName,
  status,
  isDeleted,
  editedAt,
  replyToId,
  replyToPreview,
  replyCount,
  firstReplyId,
  type,
  metadata,
  hideQuote,
  onEdit,
  onDelete,
  onReply,
}) => {
  const [isEditing, setIsEditing] = useState(false);
  const [editContent, setEditContent] = useState(content);
  const { t } = useTranslation();

  const date = new Date(timestamp);
  const hours = date.getHours().toString().padStart(2, '0');
  const minutes = date.getMinutes().toString().padStart(2, '0');
  const timeString = `${hours}:${minutes}`;

  const urls = useMemo(() => {
    return !isEditing && type !== 'image' && type !== 'file' ? extractUrls(content) : [];
  }, [content, type, isEditing]);

  const renderStatus = () => {
    if (!isOwn || !status || isDeleted) return null;
    switch (status) {
      case 'sent':
        return <span className={styles.status}>✓</span>;
      case 'delivered':
        return <span className={styles.status}>✓✓</span>;
      case 'failed':
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
    if (window.confirm(t('chat.messageBubble.deleteConfirm'))) {
      onDelete?.(id);
    }
  };

  const handleReply = () => {
    const preview = content.length > 80 ? content.slice(0, 80) + '…' : content;
    onReply?.(id, preview, senderName);
  };

  const scrollToMessage = (messageId?: string) => {
    if (!messageId) {
      return;
    }

    const element = document.getElementById(`message-${messageId}`);
    if (element) {
      element.scrollIntoView({ behavior: 'smooth', block: 'center' });
      element.classList.add(styles.highlight);
      window.setTimeout(() => {
        element.classList.remove(styles.highlight);
      }, 1500);
    }
  };

  const handleQuoteClick = () => {
    scrollToMessage(replyToId);
  };

  const handleRepliesClick = () => {
    if (replyCount && replyCount > 0) {
      scrollToMessage(firstReplyId);
    }
  };

  if (isDeleted) {
    return (
      <div
        id={`message-${id}`}
        className={`${styles.container} ${isOwn ? styles.own : styles.peer}`}
      >
        <div className={`${styles.bubble} ${styles.tombstone}`}>
          <div className={styles.content}>{t('chat.messageBubble.deleted')}</div>
        </div>
      </div>
    );
  }

  return (
    <div id={`message-${id}`} className={`${styles.container} ${isOwn ? styles.own : styles.peer}`}>
      <div className={`${styles.bubble} ${isOwn ? styles.ownBubble : styles.peerBubble}`}>
        {replyToId && replyToPreview && !hideQuote && (
          <button
            type="button"
            className={styles.quoteBubble}
            data-testid="quote-bubble"
            onClick={handleQuoteClick}
            aria-label={t('chat.messageBubble.jumpToReply', { preview: replyToPreview })}
          >
            <span className={styles.quotePreview}>{replyToPreview}</span>
          </button>
        )}
        {isEditing ? (
          <div className={styles.editContainer}>
            <textarea
              className={styles.editInput}
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  handleSave();
                } else if (e.key === 'Escape') {
                  e.preventDefault();
                  handleCancel();
                }
              }}
            />
            <div className={styles.editActions}>
              <button type="button" className={styles.saveButton} onClick={handleSave}>
                {t('chat.messageBubble.save')}
              </button>
              <button type="button" className={styles.cancelButton} onClick={handleCancel}>
                {t('chat.messageBubble.cancel')}
              </button>
            </div>
          </div>
        ) : (
          <div className={styles.content}>
            {type === 'image' ? (
              <ImageThumbnail
                filePath={metadata?.originalPath}
                thumbnailPath={metadata?.thumbnailPath}
                thumbnailState={metadata?.thumbnailState || 'pending'}
                fileName={metadata?.fileName || content}
              />
            ) : (
              <>
                <RichTextRenderer content={content} />
                {urls.length > 0 && <OgPreviewCard url={urls[0]} isOwn={isOwn} />}
              </>
            )}
          </div>
        )}

        <div className={`${styles.meta} ${isOwn ? styles.ownMeta : styles.peerMeta}`}>
          {editedAt && !isEditing && (
            <span className={styles.editedMark}>{t('chat.messageBubble.edited')}</span>
          )}
          {encrypted === true && (
            <span
              className={styles.encryptedBadge}
              data-testid="encrypted-badge"
              role="img"
              aria-label={t('chat.messageBubble.encrypted')}
              title={t('chat.messageBubble.encrypted')}
            >
              🔒
            </span>
          )}
          <span className={styles.time}>{timeString}</span>
          {renderStatus()}
        </div>

        {replyCount && replyCount > 0 && (
          <button
            type="button"
            className={styles.replyCountBadge}
            data-testid="reply-count-badge"
            onClick={handleRepliesClick}
          >
            {t('chat.messageBubble.replies', { count: replyCount })}
          </button>
        )}

        {!isEditing && (
          <div className={styles.actions} data-testid="message-actions">
            <button
              type="button"
              className={styles.actionButton}
              onClick={handleReply}
              data-testid="action-reply"
            >
              {t('chat.messageBubble.reply')}
            </button>
            {isOwn && onEdit && (
              <button
                type="button"
                className={styles.actionButton}
                onClick={() => setIsEditing(true)}
                data-testid="action-edit"
              >
                {t('chat.messageBubble.edit')}
              </button>
            )}
            {isOwn && onDelete && (
              <button
                type="button"
                className={styles.actionButton}
                onClick={handleDelete}
                data-testid="action-delete"
              >
                {t('chat.messageBubble.delete')}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
};
