import React, { useState } from 'react';
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
          <h2 className={styles.title}>Create Group</h2>
          <button type="button" className={styles.closeBtn} onClick={onClose} aria-label="Close">
            &times;
          </button>
        </div>

        <div className={styles.inputGroup}>
          <label htmlFor="groupName" className={styles.label}>
            Group Name
          </label>
          <input
            id="groupName"
            type="text"
            className={styles.input}
            placeholder="Group Name"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        <div className={styles.inputGroup}>
          <span className={styles.label}>Select Members</span>
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
              <span style={{ color: 'var(--text-secondary)' }}>No peers available</span>
            )}
          </div>
        </div>

        <div className={styles.actions}>
          <button type="button" className={styles.cancelBtn} onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className={styles.createBtn}
            onClick={handleCreate}
            disabled={!name.trim() || selectedMembers.size === 0}
          >
            Create
          </button>
        </div>
      </div>
    </div>
  );
};
