import React from "react";
import styles from "./ChatHeader.module.css";

export interface ChatHeaderProps {
  peerName: string;
  status: "online" | "offline";
  onBack?: () => void;
}

export const ChatHeader: React.FC<ChatHeaderProps> = ({ peerName, status, onBack }) => {
  return (
    <header className={styles.header}>
      {onBack && (
        <button
          className={styles.backButton}
          onClick={onBack}
          aria-label="Back"
          title="Back"
        >
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M19 12H5M12 19l-7-7 7-7" />
          </svg>
        </button>
      )}
      <div className={styles.info}>
        <div className={styles.name}>{peerName}</div>
        <div className={styles.status}>
          <span
            className={`${styles.statusIndicator} ${
              status === "online" ? styles.online : styles.offline
            }`}
          />
          {status}
        </div>
      </div>
    </header>
  );
};
