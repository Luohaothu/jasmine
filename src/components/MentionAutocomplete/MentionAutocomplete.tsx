import React, { useEffect, useRef } from "react";
import { Peer } from "../../types/peer";
import styles from "./MentionAutocomplete.module.css";

export interface MentionAutocompleteProps {
  query: string;
  peers: Peer[];
  selectedIndex: number;
  // eslint-disable-next-line no-unused-vars
  onSelect: (peer: Peer) => void;
  onDismiss?: () => void;
  position?: { bottom: string | number; left: string | number };
}

export const MentionAutocomplete: React.FC<MentionAutocompleteProps> = ({
  query,
  peers,
  selectedIndex,
  onSelect,
  onDismiss,
  position,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (containerRef.current && containerRef.current.contains(target)) {
        return;
      }
      if (target.tagName === 'TEXTAREA') {
        return;
      }
      onDismiss?.();
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
    };
  }, [onDismiss]);

  const filteredPeers = peers
    .filter((peer) => peer.name.toLowerCase().includes(query.toLowerCase()))
    .sort((a, b) => {
      if (a.status !== b.status) {
        return a.status === "online" ? -1 : 1;
      }
      return a.name.localeCompare(b.name);
    });

  useEffect(() => {
    if (containerRef.current) {
      const selectedEl = containerRef.current.children[selectedIndex] as HTMLElement;
      if (selectedEl && typeof selectedEl.scrollIntoView === 'function') {
        selectedEl.scrollIntoView({ block: "nearest" });
      }
    }
  }, [selectedIndex, filteredPeers.length]);

  if (filteredPeers.length === 0) {
    return (
      <div 
        className={styles.container} 
        style={position ? { bottom: position.bottom, left: position.left } : undefined}
        data-testid="mention-autocomplete"
      >
        <div className={styles.empty}>No matches found</div>
      </div>
    );
  }

  const validSelectedIndex = Math.min(selectedIndex, Math.max(0, filteredPeers.length - 1));

  return (
    <div 
      className={styles.container} 
      ref={containerRef} 
      style={position ? { bottom: position.bottom, left: position.left } : undefined}
      data-testid="mention-autocomplete"
    >
      {filteredPeers.map((peer, index) => (
        <div
          key={peer.id}
          className={`${styles.item} ${index === validSelectedIndex ? styles.itemSelected : ""}`}
          onMouseDown={(e) => {
            e.preventDefault();
            onSelect(peer);
          }}
          data-testid={`mention-peer-${peer.id}`}
        >
          <div className={styles.name}>{peer.name}</div>
          <div 
            className={`${styles.status} ${peer.status === "online" ? styles.statusOnline : styles.statusOffline}`}
            title={peer.status}
          />
        </div>
      ))}
    </div>
  );
};
