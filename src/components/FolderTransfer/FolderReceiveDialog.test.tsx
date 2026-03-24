import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

import '../../i18n/i18n';
import { useFolderTransferStore } from '../../stores/folderTransferStore';
import { FolderReceiveDialog } from './FolderReceiveDialog';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
}));

describe('FolderReceiveDialog', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(open).mockResolvedValue('/tmp/downloads');

    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === 'get_settings') {
        return { download_dir: '/tmp/downloads' };
      }

      return undefined;
    });
    useFolderTransferStore.setState({
      folderTransfers: [],
      activeFolderOffers: [
        {
          id: 'fo1',
          folderName: 'test-folder',
          totalSize: 1024,
          totalFiles: 5,
          senderName: 'Alice',
        },
      ],
    });
  });

  it('renders when there is an active folder offer', () => {
    render(<FolderReceiveDialog />);
    expect(screen.getByText('Folder Offer Received')).toBeInTheDocument();
    expect(screen.getByText('test-folder')).toBeInTheDocument();
    expect(screen.getByText(/1 KB/)).toBeInTheDocument();
    expect(screen.getByText(/5 files/)).toBeInTheDocument();
  });

  it('calls invoke accept_folder_transfer and removes offer on Accept', async () => {
    render(<FolderReceiveDialog />);
    const acceptBtn = screen.getByRole('button', { name: /Accept/i });
    fireEvent.click(acceptBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('get_settings');
      expect(invoke).toHaveBeenCalledWith('accept_folder_transfer', {
        folderId: 'fo1',
        targetDir: '/tmp/downloads',
      });
    });
    await waitFor(() => {
      expect(useFolderTransferStore.getState().activeFolderOffers).toHaveLength(0);
    });
  });

  it('calls invoke reject_folder_transfer and removes offer on Reject', async () => {
    render(<FolderReceiveDialog />);
    const rejectBtn = screen.getByRole('button', { name: /Reject/i });
    fireEvent.click(rejectBtn);

    expect(invoke).toHaveBeenCalledWith('reject_folder_transfer', { folderId: 'fo1' });
    await waitFor(() => {
      expect(useFolderTransferStore.getState().activeFolderOffers).toHaveLength(0);
    });
  });

  it('does not render when no offers', () => {
    useFolderTransferStore.setState({ activeFolderOffers: [] });
    const { container } = render(<FolderReceiveDialog />);
    expect(container).toBeEmptyDOMElement();
  });
});
