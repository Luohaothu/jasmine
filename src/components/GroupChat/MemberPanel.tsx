import React from 'react';
import { useTranslation } from 'react-i18next';
import styles from './MemberPanel.module.css';
import { Peer } from '../../types/peer';

/* eslint-disable no-unused-vars */
export interface MemberPanelProps {
  isOpen: boolean;
  members: Peer[];
  onAddMembers?: (memberIds: string[]) => void | Promise<void>;
  onRemoveMember?: (memberId: string) => void | Promise<void>;
  onLeaveGroup?: () => void | Promise<void>;
}
/* eslint-enable no-unused-vars */

export const MemberPanel: React.FC<MemberPanelProps> = ({
  isOpen,
  members,
  onAddMembers,
  onRemoveMember,
  onLeaveGroup,
}) => {
  const { t } = useTranslation();

  if (!isOpen) return null;

  const handleAddMembers = () => {
    if (!onAddMembers) {
      return;
    }

    const input = window.prompt(t('groups.memberPanel.addMembersPrompt'));
    if (!input) {
      return;
    }

    const memberIds = input
      .split(',')
      .map((memberId) => memberId.trim())
      .filter(Boolean);
    if (memberIds.length === 0) {
      return;
    }

    Promise.resolve(onAddMembers(memberIds)).catch((error) => {
      console.error('Failed to add group members:', error);
    });
  };

  const handleRemoveMember = (memberId: string) => {
    if (!onRemoveMember) {
      return;
    }

    Promise.resolve(onRemoveMember(memberId)).catch((error) => {
      console.error('Failed to remove group member:', error);
    });
  };

  const handleLeaveGroup = () => {
    if (!onLeaveGroup) {
      return;
    }

    Promise.resolve(onLeaveGroup()).catch((error) => {
      console.error('Failed to leave group:', error);
    });
  };

  return (
    <div className={styles.panel} data-testid="member-panel">
      <h3 className={styles.header}>{t('groups.memberPanel.title', { count: members.length })}</h3>
      {(onAddMembers || onLeaveGroup) && (
        <div style={{ display: 'flex', gap: '0.5rem', marginBottom: '0.75rem' }}>
          {onAddMembers && (
            <button type="button" onClick={handleAddMembers}>
              {t('groups.memberPanel.addMembers')}
            </button>
          )}
          {onLeaveGroup && (
            <button type="button" onClick={handleLeaveGroup}>
              {t('groups.memberPanel.leaveGroup')}
            </button>
          )}
        </div>
      )}
      <div className={styles.memberList}>
        {members.map((member) => (
          <div key={member.id} className={styles.memberItem}>
            <div style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', flex: 1 }}>
              <span
                className={`${styles.indicator} ${member.status === 'online' ? styles.online : styles.offline}`}
                data-testid="status-indicator"
                role="img"
                aria-label={t(`common.status.${member.status}`)}
              />
              <span className={styles.memberName}>{member.name}</span>
            </div>
            {onRemoveMember && (
              <button type="button" onClick={() => handleRemoveMember(member.id)}>
                {t('groups.memberPanel.remove')}
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
};
