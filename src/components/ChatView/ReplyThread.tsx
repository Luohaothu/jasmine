import React from 'react';
import { useTranslation } from 'react-i18next';
import styles from './ReplyThread.module.css';

export interface ReplyThreadProps {
  replyToId: string;
  replyToPreview: string;
  children: React.ReactNode;
}

export const ReplyThread: React.FC<ReplyThreadProps> = ({
  replyToId,
  replyToPreview,
  children,
}) => {
  const { t } = useTranslation();

  const handleQuoteClick = () => {
    const element = document.getElementById(`message-${replyToId}`);
    if (element) {
      element.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  };

  return (
    <div className={styles.threadContainer} data-testid="reply-thread">
      <div className={styles.threadHeader}>
        <button
          type="button"
          className={styles.threadQuote}
          onClick={handleQuoteClick}
          aria-label={t('chat.messageBubble.jumpToReply', { preview: replyToPreview })}
          data-testid="thread-quote"
        >
          <span className={styles.quoteIcon}>↳</span>
          <span className={styles.quotePreview}>{replyToPreview}</span>
        </button>
      </div>
      <div className={styles.threadBody}>
        <div className={styles.threadLineContainer}>
          <div className={styles.threadLine} />
        </div>
        <div className={styles.threadMessages}>{children}</div>
      </div>
    </div>
  );
};
