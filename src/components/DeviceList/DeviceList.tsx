import { useState, useMemo, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { usePeerStore } from '../../stores/peerStore';
import { DeviceListItem } from './DeviceListItem';
import { CreateGroupModal } from '../GroupChat/CreateGroupModal';
import styles from './DeviceList.module.css';

export const DeviceList = () => {
  const { t } = useTranslation();
  const peers = usePeerStore((state) => state.peers);
  const setupListeners = usePeerStore((state) => state.setupListeners);
  const [search, setSearch] = useState('');
  const [isGroupModalOpen, setIsGroupModalOpen] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    setupListeners()
      .then((cleanup) => {
        unlisten = cleanup;
      })
      .catch((error) => {
        console.error('Failed to setup peer listeners:', error);
      });

    return () => {
      if (unlisten) unlisten();
    };
  }, [setupListeners]);

  const filteredPeers = useMemo(() => {
    return peers.filter((peer) => peer.name.toLowerCase().includes(search.toLowerCase()));
  }, [peers, search]);

  return (
    <>
      <div className={`sidebar ${styles.container}`} data-testid="sidebar">
        <div className={styles.header}>
          <div className={styles.headerTop}>
            <h2 className={styles.title}>{t('devices.list.title')}</h2>
            <button
              type="button"
              className={styles.newGroupBtn}
              onClick={() => setIsGroupModalOpen(true)}
              aria-label={t('devices.list.newGroup')}
            >
              {t('devices.list.newGroupButton')}
            </button>
          </div>
          <div className={styles.searchWrapper}>
            <input
              type="text"
              placeholder={t('devices.list.searchPlaceholder')}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              className={styles.searchInput}
            />
            <span className={styles.searchIcon}>🔍</span>
          </div>
        </div>

        <div className={styles.content}>
          {peers.length === 0 ? (
            <div className={styles.emptyState}>
              <div className={styles.spinner} />
              <p className={styles.emptyText}>{t('devices.list.searching')}</p>
            </div>
          ) : (
            <ul className={styles.list}>
              {filteredPeers.map((peer) => (
                <li key={peer.id} className={styles.listItem}>
                  <DeviceListItem peer={peer} />
                </li>
              ))}
              {filteredPeers.length === 0 && (
                <p className={styles.noResults}>{t('devices.list.noResults', { search })}</p>
              )}
            </ul>
          )}
        </div>
      </div>

      <CreateGroupModal
        isOpen={isGroupModalOpen}
        onClose={() => setIsGroupModalOpen(false)}
        peers={peers}
      />
    </>
  );
};
