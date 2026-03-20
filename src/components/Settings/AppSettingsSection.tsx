import React from "react";
import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import styles from "./Settings.module.css";

interface SettingsData {
  download_dir: string;
  max_concurrent_transfers: number;
}

export default function AppSettingsSection() {
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [savedTransfers, setSavedTransfers] = useState<number>(3);

  useEffect(() => {
    invoke<SettingsData | null>("get_settings").then((res) => {
      if (res) {
        setSettings(res);
        setSavedTransfers(res.max_concurrent_transfers);
      }
    }).catch((error) => { void error; });
  }, []);

  const handleTransfersBlur = (e: React.FocusEvent<HTMLInputElement>) => {
    let val = parseInt(e.target.value, 10);
    if (isNaN(val)) val = 3;
    val = Math.max(1, Math.min(5, val));

    if (val !== savedTransfers && settings) {
      const newSettings = { ...settings, max_concurrent_transfers: val };
      invoke("update_settings", { settings: newSettings }).catch((error) => { void error; });
      setSavedTransfers(val);
      setSettings(newSettings);
    } else if (val !== settings?.max_concurrent_transfers) {
      setSettings(prev => prev ? { ...prev, max_concurrent_transfers: val } : null);
    }
  };

  const handleTransfersChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setSettings(prev => prev ? { ...prev, max_concurrent_transfers: parseInt(e.target.value, 10) || 0 } : null);
  };

  const handleChangeFolder = () => {
    open({ directory: true }).then(selected => {
      if (typeof selected === "string" && settings) {
        const newSettings = { ...settings, download_dir: selected };
        invoke("update_settings", { settings: newSettings }).then(() => {
          setSettings(newSettings);
        }).catch((error) => { void error; });
      }
    }).catch((error) => { void error; });
  };

  if (!settings) return <div className={styles.section}>Loading settings...</div>;

  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>Application Settings</h2>
      
      <div className={styles.fieldGroup}>
        <label className={styles.label}>Download Directory</label>
        <div className={styles.row}>
          <button className={styles.button} onClick={handleChangeFolder}>
            Change Folder
          </button>
          <span className={styles.aboutText} title={settings.download_dir}>
            {settings.download_dir || "Default"}
          </span>
        </div>
      </div>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="max_transfers">
          Max Concurrent Transfers (1-5)
        </label>
        <input
          id="max_transfers"
          className={styles.input}
          type="number"
          min={1}
          max={5}
          value={settings.max_concurrent_transfers}
          onChange={handleTransfersChange}
          onBlur={handleTransfersBlur}
        />
      </div>
    </section>
  );
}
