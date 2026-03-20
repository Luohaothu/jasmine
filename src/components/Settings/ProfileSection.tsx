import React from "react";
import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import styles from "./Settings.module.css";

interface Identity {
  device_id: string;
  display_name: string;
  avatar_path: string;
}

export default function ProfileSection() {
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [savedName, setSavedName] = useState<string>("");

  useEffect(() => {
    invoke<Identity | null>("get_identity").then((res) => {
      if (res) {
        setIdentity(res);
        setSavedName(res.display_name);
      }
    }).catch((error) => { void error; });
  }, []);

  const handleNameBlur = (e: React.FocusEvent<HTMLInputElement>) => {
    const name = e.target.value;
    if (name !== savedName) {
      invoke("update_display_name", { name }).catch((error) => { void error; });
      setSavedName(name);
    }
  };

  const handleNameChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setIdentity(prev => prev ? { ...prev, display_name: e.target.value } : null);
  };

  const handleChangeAvatar = () => {
    open({
      filters: [{ name: "Images", extensions: ["jpg", "png", "gif"] }]
    }).then(selected => {
      if (typeof selected === "string") {
        invoke("update_avatar", { path: selected }).then(() => {
          setIdentity(prev => prev ? { ...prev, avatar_path: selected } : null);
        }).catch((error) => { void error; });
      }
    }).catch((error) => { void error; });
  };

  const copyDeviceId = () => {
    if (identity?.device_id) {
      navigator.clipboard.writeText(identity.device_id).catch((error) => { void error; });
    }
  };

  if (!identity) return <div className={styles.section}>Loading profile...</div>;

  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>Profile Information</h2>
      
      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="device_id">Device ID</label>
        <div className={styles.row}>
          <input
            id="device_id"
            className={styles.input}
            type="text"
            value={identity.device_id}
            readOnly
            title="Device ID"
          />
          <button 
            className={styles.iconButton} 
            onClick={copyDeviceId} 
            aria-label="Copy Device ID"
            title="Copy"
            type="button"
          >
            Copy
          </button>
        </div>
      </div>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="display_name">Display Name</label>
        <input
          id="display_name"
          className={styles.input}
          type="text"
          value={identity.display_name}
          onChange={handleNameChange}
          onBlur={handleNameBlur}
        />
      </div>

      <div className={styles.fieldGroup}>
        <label className={styles.label}>Avatar</label>
        <div className={styles.row}>
          <button className={styles.button} onClick={handleChangeAvatar} type="button">
            Change Avatar
          </button>
          <span className={styles.aboutText}>{identity.avatar_path || "No avatar set"}</span>
        </div>
      </div>
    </section>
  );
}
