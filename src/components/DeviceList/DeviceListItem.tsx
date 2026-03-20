import { Link } from "react-router-dom";
import styles from "./DeviceListItem.module.css";
import { Peer } from "../../types/peer";

interface Props {
  peer: Peer;
}

export const DeviceListItem = ({ peer }: Props) => {
  const initial = peer.name ? peer.name.charAt(0).toUpperCase() : "?";

  return (
    <Link to={`/chat/${peer.id}`} className={styles.item} aria-label={peer.name}>
      <div className={styles.avatar}>
        {initial}
      </div>
      <div className={styles.info}>
        <span className={styles.name}>{peer.name}</span>
        <div className={styles.status}>
          <span className={`${styles.dot} ${styles[peer.status]}`} />
          <span className={styles.statusText}>{peer.status}</span>
        </div>
      </div>
    </Link>
  );
};
