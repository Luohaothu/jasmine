import React from "react";
import styles from "./MemberPanel.module.css";
import { Peer } from "../../types/peer";

export interface MemberPanelProps {
  isOpen: boolean;
  members: Peer[];
}

export const MemberPanel: React.FC<MemberPanelProps> = ({ isOpen, members }) => {
  if (!isOpen) return null;

  return (
    <div className={styles.panel} data-testid="member-panel">
      <h3 className={styles.header}>Members ({members.length})</h3>
      <div className={styles.memberList}>
        {members.map((member) => (
          <div key={member.id} className={styles.memberItem}>
            <span
              className={`${styles.indicator} ${member.status === "online" ? styles.online : styles.offline}`}
              data-testid="status-indicator"
              aria-label={member.status}
            />
            <span className={styles.memberName}>{member.name}</span>
          </div>
        ))}
      </div>
    </div>
  );
};
