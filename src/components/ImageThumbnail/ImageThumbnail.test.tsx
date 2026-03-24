import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { ImageThumbnail } from './ImageThumbnail';
import * as tauriCore from '@tauri-apps/api/core';
import * as tauriOpener from '@tauri-apps/plugin-opener';
import '../../i18n/i18n';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: vi.fn((path) => `asset://${path}`),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openPath: vi.fn(),
}));

describe('ImageThumbnail', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders pending state correctly', () => {
    render(<ImageThumbnail thumbnailState="pending" />);
    expect(screen.getByTestId('image-thumbnail-pending')).toBeInTheDocument();
    expect(screen.getByText('Generating thumbnail...')).toBeInTheDocument();
  });

  it('renders failed state correctly', () => {
    render(<ImageThumbnail thumbnailState="failed" fileName="broken.png" />);
    expect(screen.getByTestId('image-thumbnail-failed')).toBeInTheDocument();
    expect(screen.getByText('broken.png')).toBeInTheDocument();
    expect(screen.getByText('🖼️')).toBeInTheDocument();
  });

  it('renders ready state and image when thumbnailPath is provided', async () => {
    render(
      <ImageThumbnail
        thumbnailState="ready"
        thumbnailPath="/fake/path/thumb.jpg"
        fileName="photo.jpg"
      />
    );

    expect(screen.getByTestId('image-thumbnail-ready')).toBeInTheDocument();

    const img = screen.getByRole('img');
    expect(img).toHaveAttribute('src', 'asset:///fake/path/thumb.jpg');
    expect(img).toHaveAttribute('alt', 'photo.jpg');
    expect(tauriCore.convertFileSrc).toHaveBeenCalledWith('/fake/path/thumb.jpg', 'asset');
  });

  it('opens the original file when clicked and filePath is provided', async () => {
    render(
      <ImageThumbnail
        thumbnailState="ready"
        thumbnailPath="/fake/path/thumb.jpg"
        filePath="/fake/path/original.jpg"
      />
    );

    const container = screen.getByTestId('image-thumbnail-ready');
    fireEvent.click(container);

    await waitFor(() => {
      expect(tauriOpener.openPath).toHaveBeenCalledWith('/fake/path/original.jpg');
    });
  });

  it('does not call openPath when clicked if filePath is missing', async () => {
    render(<ImageThumbnail thumbnailState="ready" thumbnailPath="/fake/path/thumb.jpg" />);

    const container = screen.getByTestId('image-thumbnail-ready');
    fireEvent.click(container);

    await waitFor(() => {
      expect(tauriOpener.openPath).not.toHaveBeenCalled();
    });
  });
});
