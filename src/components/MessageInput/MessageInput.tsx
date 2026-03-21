import React, { useRef, useState, useEffect, useCallback, KeyboardEvent, DragEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePeerStore } from "../../stores/peerStore";
import { useChatStore } from "../../stores/chatStore";
import { Peer } from "../../types/peer";
import { MentionAutocomplete } from "../MentionAutocomplete/MentionAutocomplete";
import styles from "./MessageInput.module.css";

export interface MessageInputProps {
  peerId?: string;
  // eslint-disable-next-line no-unused-vars
  onSend?: (content: string, replyToId?: string) => void | Promise<void>;
  disabled?: boolean;
}

const MAX_CHARS = 10000;
const WARNING_THRESHOLD = 500;

export const MessageInput: React.FC<MessageInputProps> = ({ peerId, onSend, disabled }) => {
  const [content, setContent] = useState("");
  const [isDragging, setIsDragging] = useState(false);
  
  const peers = usePeerStore((state) => state.peers);
  const replyingTo = useChatStore((state) => state.replyingTo);
  const setReplyingTo = useChatStore((state) => state.setReplyingTo);

  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [mentionStartIndex, setMentionStartIndex] = useState<number | null>(null);
  const [mentionSelectedIndex, setMentionSelectedIndex] = useState(0);
  const [dismissedMentionIndex, setDismissedMentionIndex] = useState<number | null>(null);
  const [caretPos, setCaretPos] = useState({ left: 12, bottom: 40 });

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const typingTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const filteredPeers = mentionQuery !== null
    ? peers
        .filter((peer) => peer.name.toLowerCase().includes(mentionQuery.toLowerCase()))
        .sort((a, b) => {
          if (a.status !== b.status) {
            return a.status === "online" ? -1 : 1;
          }
          return a.name.localeCompare(b.name);
        })
    : [];

  const handleSend = useCallback(async () => {
    const trimmed = content.trim();
    if (!trimmed) return;

    try {
      if (onSend) {
        await onSend(trimmed, replyingTo?.id);
      } else if (peerId) {
        await invoke("send_message", { 
          peerId, 
          content: trimmed,
          replyToId: replyingTo?.id || null 
        });
      }
      setContent("");
      setMentionQuery(null);
      setDismissedMentionIndex(null);
      setReplyingTo(null);
      if (textareaRef.current) {
        textareaRef.current.style.height = "auto";
      }
    } catch (error) {
      console.error("Message send failed:", error);
    }
  }, [content, peerId, onSend, replyingTo, setReplyingTo]);

  const insertMention = (peer: Peer) => {
    if (mentionStartIndex === null) return;
    
    const beforeMention = content.substring(0, mentionStartIndex - 1);
    const afterMention = content.substring(textareaRef.current?.selectionStart || content.length);
    
    const mentionText = `@[${peer.name}](user:${peer.id}) `;
    const newContent = beforeMention + mentionText + afterMention;
    
    setContent(newContent);
    setMentionQuery(null);
    setMentionStartIndex(null);
    setDismissedMentionIndex(null);
    
    setTimeout(() => {
      if (textareaRef.current) {
        textareaRef.current.focus();
        const newCursorPos = beforeMention.length + mentionText.length;
        textareaRef.current.setSelectionRange(newCursorPos, newCursorPos);
      }
    }, 0);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (mentionQuery !== null) {
      if (e.key === "Escape") {
        e.preventDefault();
        setMentionQuery(null);
        setDismissedMentionIndex(mentionStartIndex);
        return;
      }
      
      if (filteredPeers.length > 0) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setMentionSelectedIndex((prev) => (prev + 1) % filteredPeers.length);
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setMentionSelectedIndex((prev) => (prev - 1 + filteredPeers.length) % filteredPeers.length);
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          insertMention(filteredPeers[mentionSelectedIndex]);
          return;
        }
      }
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const checkMentionTrigger = (val: string, cursorPos: number) => {
    const textBeforeCursor = val.substring(0, cursorPos);
    const match = /(?:^|\s)@([^@\s]*)$/.exec(textBeforeCursor);
    
    if (textareaRef.current) {
      const el = textareaRef.current;
      
      const lines = textBeforeCursor.split('\n');
      const currentLineIndex = lines.length - 1;
      const currentLineText = lines[currentLineIndex] || '';
      
      const computedStyle = window.getComputedStyle(el);
      const lineHeight = parseFloat(computedStyle.lineHeight) || 20;
      
      const charWidth = 8;
      const left = Math.min(Math.max(currentLineText.length * charWidth, 12), el.clientWidth - 200);
      
      const totalLines = val.split('\n').length;
      const linesFromBottom = Math.max(0, totalLines - 1 - currentLineIndex);
      
      const bottomOffset = 100 + (linesFromBottom * lineHeight);
      
      setCaretPos({ left, bottom: bottomOffset });
    }

    if (match) {
      const matchIndex = textBeforeCursor.lastIndexOf("@");
      const startIndex = matchIndex + 1;
      
      if (startIndex !== dismissedMentionIndex) {
        setMentionQuery((prevQuery) => {
          if (prevQuery !== match[1] || mentionStartIndex !== startIndex) {
            setMentionSelectedIndex(0);
          }
          return match[1];
        });
        setMentionStartIndex(startIndex);
      }
    } else {
      setMentionQuery(null);
      setMentionStartIndex(null);
      setDismissedMentionIndex(null);
    }
  };

  const handleSelectionChange = () => {
    if (textareaRef.current) {
      checkMentionTrigger(content, textareaRef.current.selectionStart);
    }
  };

  const handleChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const newVal = e.target.value;
    if (newVal.length > MAX_CHARS) return;
    
    setContent(newVal);
    
    const target = e.target;
    target.style.height = "auto";
    target.style.height = `${Math.min(target.scrollHeight, 120)}px`;

    checkMentionTrigger(newVal, target.selectionStart);

    if (typingTimeoutRef.current) {
      clearTimeout(typingTimeoutRef.current);
    }
    
    typingTimeoutRef.current = setTimeout(() => {
      if (peerId) {
        invoke("send_typing_indicator", { peerId }).catch((error) => {
          console.warn("Failed to send typing indicator:", error);
        });
      }
    }, 500);
  };

  useEffect(() => {
    return () => {
      if (typingTimeoutRef.current) {
        clearTimeout(typingTimeoutRef.current);
      }
    };
  }, []);

  const handleDragOver = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    if (!e.dataTransfer.types.includes("Files")) return;
    setIsDragging(true);
  };

  const handleDragLeave = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    const relatedTarget = e.relatedTarget as HTMLElement;
    if (!e.currentTarget.contains(relatedTarget)) {
      setIsDragging(false);
    }
  };

  const handleDrop = async (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setIsDragging(false);
    
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      const filePaths = Array.from(e.dataTransfer.files).map(f => f.name);
      if (filePaths.length > 0) {
        try {
          if (peerId) {
            for (const filePath of filePaths) {
              await invoke("send_file", { peerId, filePath });
            }
          }
        } catch (error) {
          console.error("Failed to send files:", error);
        }
      }
    }
  };

  const charsRemaining = MAX_CHARS - content.length;
  const isNearLimit = charsRemaining <= WARNING_THRESHOLD;
  const isDisabled = !content.trim() || disabled;

  return (
    <div 
      className={styles.container} 
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
      data-testid="message-input-container"
    >
      {isDragging && (
        <div className={styles.dropOverlay}>
          释放以发送文件
        </div>
      )}
      
      {replyingTo && (
        <div className={styles.replyPreviewBar} data-testid="reply-preview-bar">
          <div className={styles.replyPreviewContent}>
            <span className={styles.replyPreviewLabel}>回复 {replyingTo.senderName || ''}:</span>
            <span className={styles.replyPreviewText}>{replyingTo.preview}</span>
          </div>
          <button 
            className={styles.cancelReplyButton} 
            onClick={() => setReplyingTo(null)}
            aria-label="Cancel reply"
            data-testid="cancel-reply-button"
          >
            ×
          </button>
        </div>
      )}

      <div className={styles.inputWrapper}>
        {mentionQuery !== null && (
          <MentionAutocomplete
            query={mentionQuery}
            peers={peers}
            selectedIndex={mentionSelectedIndex}
            onSelect={insertMention}
            position={{ left: caretPos.left, bottom: caretPos.bottom }}
            onDismiss={() => {
              setMentionQuery(null);
              setDismissedMentionIndex(mentionStartIndex);
            }}
          />
        )}
        <textarea
          ref={textareaRef}
          className={styles.textarea}
          value={content}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          onClick={handleSelectionChange}
          onKeyUp={handleSelectionChange}
          placeholder="Type a message..."
          rows={1}
          maxLength={MAX_CHARS}
        />
        {isNearLimit && (
          <div className={`${styles.charCount} ${charsRemaining === 0 ? styles.charCountLimit : ''}`}>
            {charsRemaining}
          </div>
        )}
        <button 
          className={styles.sendButton} 
          onClick={handleSend} 
          disabled={isDisabled}
          aria-label="Send"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={styles.icon}>
            <line x1="22" y1="2" x2="11" y2="13"></line>
            <polygon points="22 2 15 22 11 13 2 9 22 2"></polygon>
          </svg>
        </button>
      </div>
    </div>
  );
};
