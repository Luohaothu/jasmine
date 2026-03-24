import React, { useState, useEffect } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { openPath } from '@tauri-apps/plugin-opener';
import { useTranslation } from 'react-i18next';
import styles from './ImageThumbnail.module.css';

export interface ImageThumbnailProps {
  filePath?: string;
  thumbnailPath?: string;
  thumbnailState: 'pending' | 'ready' | 'failed';
  fileName?: string;
  alt?: string;
}

export const ImageThumbnail: React.FC<ImageThumbnailProps> = ({
  filePath,
  thumbnailPath,
  thumbnailState,
  fileName,
  alt,
}) => {
  const [imgSrc, setImgSrc] = useState<string | null>(null);
  const { t } = useTranslation();

  useEffect(() => {
    if (thumbnailState === 'ready' && thumbnailPath) {
      try {
        const url = convertFileSrc(thumbnailPath, 'asset');
        setImgSrc(url);
      } catch (err) {
        console.error('Failed to convert file src', err);
      }
    }
  }, [thumbnailPath, thumbnailState]);

  const handleOpen = async () => {
    if (filePath) {
      try {
        await openPath(filePath);
      } catch (err) {
        console.error('Failed to open file', err);
      }
    }
  };

  if (thumbnailState === 'pending') {
    return (
      <div
        className={`${styles.container} ${styles.pending}`}
        data-testid="image-thumbnail-pending"
      >
        <div className={styles.spinner}></div>
        <span className={styles.statusText}>{t('common.imageThumbnail.generating')}</span>
      </div>
    );
  }

  if (thumbnailState === 'failed' || !imgSrc) {
    return (
      <div className={`${styles.container} ${styles.failed}`} data-testid="image-thumbnail-failed">
        <div className={styles.fallbackIcon}>🖼️</div>
        <span className={styles.statusText}>{fileName || t('common.imageThumbnail.image')}</span>
      </div>
    );
  }

  if (filePath) {
    return (
      <button
        type="button"
        className={`${styles.container} ${styles.interactiveButton}`}
        onClick={handleOpen}
        title={t('common.imageThumbnail.openInSystemViewer')}
        data-testid="image-thumbnail-ready"
        aria-label={t('common.imageThumbnail.openImage', {
          name: fileName || t('common.imageThumbnail.image'),
        })}
      >
        <img
          src={imgSrc}
          alt={alt || fileName || t('common.imageThumbnail.alt')}
          className={styles.image}
        />
      </button>
    );
  }

  return (
    <div className={styles.container} data-testid="image-thumbnail-ready">
      <img
        src={imgSrc}
        alt={alt || fileName || t('common.imageThumbnail.alt')}
        className={styles.image}
      />
    </div>
  );
};
