import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import styles from './OgPreviewCard.module.css';

export interface OgMetadata {
  url: string;
  title: string | null;
  description: string | null;
  image_url: string | null;
  site_name: string | null;
}

export interface OgPreviewCardProps {
  url: string;
  isOwn?: boolean;
}

interface OgUpdatedPayload {
  url: string;
  metadata: OgMetadata;
}

function hasRenderableMetadata(metadata: OgMetadata): boolean {
  return Boolean(
    metadata.title || metadata.description || metadata.image_url || metadata.site_name
  );
}

export const OgPreviewCard: React.FC<OgPreviewCardProps> = ({ url, isOwn }) => {
  const { t } = useTranslation();
  const [metadata, setMetadata] = useState<OgMetadata | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [hasError, setHasError] = useState(false);

  useEffect(() => {
    let isMounted = true;

    const applyMetadata = (data: OgMetadata) => {
      if (!isMounted) {
        return;
      }

      if (hasRenderableMetadata(data)) {
        setMetadata(data);
        setHasError(false);
      } else {
        setMetadata(null);
        setHasError(true);
      }

      setIsLoading(false);
    };

    async function fetchMetadata() {
      setMetadata(null);
      setIsLoading(true);
      setHasError(false);

      try {
        const data = await invoke<OgMetadata>('fetch_og_metadata', { url });

        applyMetadata(data);
      } catch (err) {
        if (isMounted) {
          console.error('Failed to fetch OG metadata:', err);
          setMetadata(null);
          setHasError(true);
          setIsLoading(false);
        }
      }
    }

    const setupPromise = (async () => {
      const unlisten = await listen<OgUpdatedPayload>('og:updated', (event) => {
        if (event.payload.url !== url) {
          return;
        }

        applyMetadata(event.payload.metadata);
      }).catch((error) => {
        console.error('Failed to listen for og:updated:', error);
        return undefined;
      });

      if (!isMounted) {
        unlisten?.();
        return undefined;
      }

      await fetchMetadata();
      return unlisten;
    })();

    return () => {
      isMounted = false;

      setupPromise
        .then((unlisten) => unlisten?.())
        .catch((error) => {
          console.error('Failed to unlisten og:updated:', error);
        });
    };
  }, [url]);

  if (hasError) {
    return null;
  }

  const cardClass = `${styles.card} ${isOwn ? styles.cardOwn : ''}`;
  const imageContainerClass = `${styles.imageContainer} ${isOwn ? styles.imageContainerOwn : ''}`;
  const skeletonTextClass = `${styles.skeletonText} ${isOwn ? styles.skeletonTextOwn : ''}`;

  if (isLoading) {
    return (
      <div className={cardClass} data-testid="og-preview-loading">
        <div className={`${imageContainerClass} ${styles.skeleton}`} />
        <div className={styles.content}>
          <div className={`${skeletonTextClass} ${styles.skeleton} ${styles.skeletonTitle}`} />
          <div className={`${skeletonTextClass} ${styles.skeleton} ${styles.skeletonDesc1}`} />
          <div className={`${skeletonTextClass} ${styles.skeleton} ${styles.skeletonDesc2}`} />
        </div>
      </div>
    );
  }

  if (!metadata) return null;

  return (
    <a
      href={metadata.url}
      target="_blank"
      rel="noopener noreferrer"
      className={cardClass}
      data-testid="og-preview-card"
    >
      {metadata.image_url && (
        <div className={imageContainerClass}>
          <img
            src={metadata.image_url}
            alt={metadata.title || t('chat.ogPreviewCard.previewAlt')}
            className={styles.image}
            onError={(e) => {
              (e.currentTarget.parentElement as HTMLElement).style.display = 'none';
            }}
          />
        </div>
      )}
      <div className={styles.content}>
        {metadata.site_name && <span className={styles.siteName}>{metadata.site_name}</span>}
        {metadata.title && <span className={styles.title}>{metadata.title}</span>}
        {metadata.description && <span className={styles.description}>{metadata.description}</span>}
      </div>
    </a>
  );
};
