import React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import styles from './Settings.module.css';

interface Identity {
  device_id: string;
  display_name: string;
  avatar_path: string;
}

export default function ProfileSection() {
  const { t } = useTranslation();
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [savedName, setSavedName] = useState<string>('');

  useEffect(() => {
    invoke<Identity | null>('get_identity')
      .then((res) => {
        if (res) {
          setIdentity(res);
          setSavedName(res.display_name);
        }
      })
      .catch((error) => {
        void error;
      });
  }, []);

  const handleNameBlur = (e: React.FocusEvent<HTMLInputElement>) => {
    const name = e.target.value;
    if (name !== savedName) {
      invoke('update_display_name', { name }).catch((error) => {
        void error;
      });
      setSavedName(name);
    }
  };

  const handleNameChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setIdentity((prev) => (prev ? { ...prev, display_name: e.target.value } : null));
  };

  const handleChangeAvatar = () => {
    open({
      filters: [
        { name: t('settings.profile.avatarFilterName'), extensions: ['jpg', 'png', 'gif'] },
      ],
    })
      .then((selected) => {
        if (typeof selected === 'string') {
          invoke('update_avatar', { path: selected })
            .then(() => {
              setIdentity((prev) => (prev ? { ...prev, avatar_path: selected } : null));
            })
            .catch((error) => {
              void error;
            });
        }
      })
      .catch((error) => {
        void error;
      });
  };

  const copyDeviceId = () => {
    if (identity?.device_id) {
      navigator.clipboard.writeText(identity.device_id).catch((error) => {
        void error;
      });
    }
  };

  if (!identity) return <div className={styles.section}>{t('settings.profile.loading')}</div>;

  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>{t('settings.profile.title')}</h2>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="device_id">
          {t('settings.profile.deviceIdLabel')}
        </label>
        <div className={styles.row}>
          <input
            id="device_id"
            className={styles.input}
            type="text"
            value={identity.device_id}
            readOnly
            title={t('settings.profile.deviceIdLabel')}
          />
          <button
            className={styles.iconButton}
            onClick={copyDeviceId}
            aria-label={t('settings.profile.copyDeviceIdAriaLabel')}
            title={t('settings.actions.copy')}
            type="button"
          >
            {t('settings.actions.copy')}
          </button>
        </div>
      </div>

      <div className={styles.fieldGroup}>
        <label className={styles.label} htmlFor="display_name">
          {t('settings.profile.displayNameLabel')}
        </label>
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
        <label className={styles.label} htmlFor="change-avatar">
          {t('settings.profile.avatarLabel')}
        </label>
        <div className={styles.row}>
          <button
            id="change-avatar"
            className={styles.button}
            onClick={handleChangeAvatar}
            type="button"
          >
            {t('settings.actions.changeAvatar')}
          </button>
          <span className={styles.aboutText}>
            {identity.avatar_path || t('settings.profile.noAvatar')}
          </span>
        </div>
      </div>
    </section>
  );
}
