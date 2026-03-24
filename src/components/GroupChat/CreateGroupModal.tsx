import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import styles from './CreateGroupModal.module.css';
import { useGroupStore } from '../../stores/groupStore';
import { Peer } from '../../types/peer';

export interface CreateGroupModalProps {
  isOpen: boolean;
  onClose: () => void;
  peers: Peer[];
}

export const CreateGroupModal: React.FC<CreateGroupModalProps> = ({ isOpen, onClose, peers }) => {
  const [name, setName] = useState('');
  const [selectedMembers, setSelectedMembers] = useState<Set<string>>(new Set());
  const createGroup = useGroupStore((state) => state.createGroup);
  const { t } = useTranslation();

  if (!isOpen) return null;

  const toggleMember = (id: string) => {
    const newSelection = new Set(selectedMembers);
    if (newSelection.has(id)) {
      newSelection.delete(id);
    } else {
      newSelection.add(id);
    }
    setSelectedMembers(newSelection);
  };

  const handleCreate = async () => {
    if (!name.trim()) return;
    try {
      await createGroup(name.trim(), Array.from(selectedMembers));
      setName('');
      setSelectedMembers(new Set());
      onClose();
    } catch (error) {
      console.error('Failed to create group:', error);
    }
  };

  return (
    <div className={styles.overlay}>
      <div className={styles.modal} role="dialog" aria-modal="true">
        <div className={styles.header}>
          <h2 className={styles.title}>{t('groups.createGroupModal.title')}</h2>
          <button
            type="button"
            className={styles.closeBtn}
            onClick={onClose}
            aria-label={t('common.actions.close')}
          >
            &times;
          </button>
        </div>

        <div className={styles.inputGroup}>
          <label htmlFor="groupName" className={styles.label}>
            {t('groups.createGroupModal.nameLabel')}
          </label>
          <input
            id="groupName"
            type="text"
            className={styles.input}
            placeholder={t('groups.createGroupModal.namePlaceholder')}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        <div className={styles.inputGroup}>
          <span className={styles.label}>{t('groups.createGroupModal.selectMembers')}</span>
          <div className={styles.memberList}>
            {peers.map((peer) => (
              <label key={peer.id} className={styles.memberItem}>
                <input
                  type="checkbox"
                  className={styles.checkbox}
                  checked={selectedMembers.has(peer.id)}
                  onChange={() => toggleMember(peer.id)}
                  aria-label={peer.name}
                />
                <span>{peer.name}</span>
              </label>
            ))}
            {peers.length === 0 && (
              <span style={{ color: 'var(--text-secondary)' }}>
                {t('groups.createGroupModal.noPeersAvailable')}
              </span>
            )}
          </div>
        </div>

        <div className={styles.actions}>
          <button type="button" className={styles.cancelBtn} onClick={onClose}>
            {t('common.actions.cancel')}
          </button>
          <button
            type="button"
            className={styles.createBtn}
            onClick={handleCreate}
            disabled={!name.trim() || selectedMembers.size === 0}
          >
            {t('common.actions.create')}
          </button>
        </div>
      </div>
    </div>
  );
};
