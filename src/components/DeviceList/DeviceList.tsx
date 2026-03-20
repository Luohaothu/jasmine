import { useState, useMemo, useEffect } from "react";
import { usePeerStore } from "../../stores/peerStore";
import { DeviceListItem } from "./DeviceListItem";
import { CreateGroupModal } from "../GroupChat/CreateGroupModal";
import styles from "./DeviceList.module.css";

export const DeviceList = () => {
  const peers = usePeerStore((state) => state.peers);
  const setupListeners = usePeerStore((state) => state.setupListeners);
  const [search, setSearch] = useState("");
  const [isGroupModalOpen, setIsGroupModalOpen] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    
    setupListeners().then((cleanup) => {
      unlisten = cleanup;
    }).catch((error) => {
      void error;
    });

    return () => {
      if (unlisten) unlisten();
    };
  }, [setupListeners]);

  const filteredPeers = useMemo(() => {
    return peers.filter((peer) =>
      peer.name.toLowerCase().includes(search.toLowerCase())
    );
  }, [peers, search]);

  return (
    <>
      <div className={`sidebar ${styles.container}`} data-testid="sidebar">
        <div className={styles.header}>
          <div className={styles.headerTop}>
            <h2 className={styles.title}>Jasmine LAN Chat</h2>
            <button 
              className={styles.newGroupBtn} 
              onClick={() => setIsGroupModalOpen(true)}
              aria-label="New Group"
            >
              + Group
            </button>
          </div>
          <div className={styles.searchWrapper}>
            <input
              type="text"
              placeholder="Search devices..."
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
              <p className={styles.emptyText}>正在搜索局域网设备...</p>
            </div>
          ) : (
            <ul className={styles.list}>
              {filteredPeers.map((peer) => (
                <li key={peer.id} className={styles.listItem}>
                  <DeviceListItem peer={peer} />
                </li>
              ))}
              {filteredPeers.length === 0 && (
                <p className={styles.noResults}>No devices found matching &quot;{search}&quot;</p>
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
