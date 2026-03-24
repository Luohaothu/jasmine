import { fireEvent, render, screen, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { invoke } from '@tauri-apps/api/core';

import '../../i18n/i18n';
import { useFolderTransferStore } from '../../stores/folderTransferStore';
import { FolderTransferPanel } from './FolderTransferPanel';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

describe('FolderTransferPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useFolderTransferStore.setState({
      folderTransfers: [],
      activeFolderOffers: [],
    });
  });

  it('does not render when no transfers', () => {
    const { container } = render(<FolderTransferPanel />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders fab and badge correctly', () => {
    useFolderTransferStore.setState({
      folderTransfers: [
        {
          id: '1',
          folderName: 'active_folder',
          totalSize: 100,
          totalFiles: 2,
          transferredBytes: 50,
          transferredFiles: 1,
          speed: 10,
          state: 'active',
          files: [],
        },
        {
          id: '2',
          folderName: 'queued_folder',
          totalSize: 200,
          totalFiles: 3,
          transferredBytes: 0,
          transferredFiles: 0,
          speed: 0,
          state: 'queued',
          files: [],
        },
        {
          id: '3',
          folderName: 'done_folder',
          totalSize: 300,
          totalFiles: 4,
          transferredBytes: 300,
          transferredFiles: 4,
          speed: 0,
          state: 'completed',
          files: [],
        },
      ],
    });

    render(<FolderTransferPanel />);
    const fab = screen.getByRole('button', { name: /Toggle folder transfer panel/i });
    expect(fab).toBeInTheDocument();

    const badge = screen.getByTestId('folder-active-badge');
    expect(badge).toHaveTextContent('2');
  });

  it('toggles panel visibility', () => {
    useFolderTransferStore.setState({
      folderTransfers: [
        {
          id: '1',
          folderName: 'f',
          totalSize: 10,
          totalFiles: 1,
          transferredBytes: 1,
          transferredFiles: 0,
          speed: 5,
          state: 'active',
          files: [],
        },
      ],
    });

    render(<FolderTransferPanel />);
    expect(screen.queryByTestId('folder-transfer-panel')).not.toBeInTheDocument();

    const fab = screen.getByRole('button', { name: /Toggle folder transfer panel/i });
    fireEvent.click(fab);

    expect(screen.getByTestId('folder-transfer-panel')).toBeInTheDocument();

    const closeBtn = screen.getByRole('button', { name: /Minimize panel/i });
    fireEvent.click(closeBtn);

    expect(screen.queryByTestId('folder-transfer-panel')).not.toBeInTheDocument();
  });

  describe('FolderTransferPanel.progress', () => {
    it('displays correct progress, speed, and status for different states', () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't1',
            folderName: 'f1',
            totalSize: 1024,
            totalFiles: 2,
            transferredBytes: 512,
            transferredFiles: 1,
            speed: 512,
            state: 'active',
            files: [],
          },
          {
            id: 't2',
            folderName: 'f2',
            totalSize: 2048,
            totalFiles: 3,
            transferredBytes: 2048,
            transferredFiles: 3,
            speed: 0,
            state: 'completed',
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const t1 = screen.getByTestId('folder-transfer-item-t1');
      expect(within(t1).getByText('active')).toBeInTheDocument();
      expect(within(t1).getByText('512 B / 1 KB')).toBeInTheDocument();
      expect(within(t1).getByText('512 B/s')).toBeInTheDocument();

      const t2 = screen.getByTestId('folder-transfer-item-t2');
      expect(within(t2).getByText('completed')).toBeInTheDocument();
      expect(within(t2).getByText('2 KB / 2 KB')).toBeInTheDocument();
      expect(within(t2).queryByText('B/s')).not.toBeInTheDocument();
    });
  });

  describe('FolderTransferPanel.cancel', () => {
    it('calls invoke with cancel_folder_transfer on cancel button click', async () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't1',
            folderName: 'f1',
            totalSize: 100,
            totalFiles: 1,
            transferredBytes: 10,
            transferredFiles: 0,
            speed: 10,
            state: 'active',
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const cancelBtn = screen.getByRole('button', { name: /Cancel transfer/i });
      fireEvent.click(cancelBtn);

      expect(invoke).toHaveBeenCalledWith('cancel_folder_transfer', { folderId: 't1' });
    });
  });

  describe('FolderTransferPanel.resumeRetry', () => {
    it('shows Resume and Retry for failed resumable folder transfers and invokes both actions', async () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't-resume',
            folderName: 'resume_folder',
            totalSize: 100,
            totalFiles: 2,
            transferredBytes: 40,
            transferredFiles: 0,
            speed: 0,
            state: 'failed',
            resumable: true,
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const item = screen.getByTestId('folder-transfer-item-t-resume');

      const resumeBtn = within(item).getByRole('button', { name: /Resume transfer/i });
      fireEvent.click(resumeBtn);
      expect(invoke).toHaveBeenCalledWith('resume_transfer', { transferId: 't-resume' });
    });

    it('shows Retry and invokes retry action', async () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't-retry',
            folderName: 'retry_folder',
            totalSize: 100,
            totalFiles: 2,
            transferredBytes: 40,
            transferredFiles: 0,
            speed: 0,
            state: 'failed',
            resumable: true,
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const item = screen.getByTestId('folder-transfer-item-t-retry');
      const retryBtn = within(item).getByRole('button', { name: /Retry transfer/i });
      fireEvent.click(retryBtn);
      expect(invoke).toHaveBeenCalledWith('retry_transfer', { transferId: 't-retry' });
    });

    it('shows Retry only for failed non-resumable transfers and keeps active cancel behavior', () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't-failed',
            folderName: 'failed_folder',
            totalSize: 100,
            totalFiles: 2,
            transferredBytes: 50,
            transferredFiles: 1,
            speed: 0,
            state: 'failed',
            files: [],
          },
          {
            id: 't-active',
            folderName: 'active_folder',
            totalSize: 100,
            totalFiles: 2,
            transferredBytes: 50,
            transferredFiles: 1,
            speed: 10,
            state: 'active',
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const failedItem = screen.getByTestId('folder-transfer-item-t-failed');
      expect(
        within(failedItem).queryByRole('button', { name: /Resume transfer/i })
      ).not.toBeInTheDocument();
      expect(
        within(failedItem).getByRole('button', { name: /Retry transfer/i })
      ).toBeInTheDocument();
      expect(
        within(failedItem).queryByRole('button', { name: /Cancel transfer/i })
      ).not.toBeInTheDocument();

      const activeItem = screen.getByTestId('folder-transfer-item-t-active');
      expect(
        within(activeItem).getByRole('button', { name: /Cancel transfer/i })
      ).toBeInTheDocument();
      expect(
        within(activeItem).queryByRole('button', { name: /Resume transfer/i })
      ).not.toBeInTheDocument();
      expect(
        within(activeItem).queryByRole('button', { name: /Retry transfer/i })
      ).not.toBeInTheDocument();
    });
  });

  describe('FolderTransferPanel.expand', () => {
    it('expands to show linked file details from the store', () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't1',
            folderName: 'f1',
            totalSize: 100,
            totalFiles: 5,
            transferredBytes: 10,
            transferredFiles: 2,
            speed: 10,
            state: 'active',
            files: [
              {
                id: 'file-1',
                filename: 'hello.txt',
                relativePath: 'docs/hello.txt',
                size: 100,
                transferredBytes: 50,
                progress: 0.5,
                speed: 10,
                state: 'active',
              },
              {
                id: 'file-2',
                filename: 'readme.txt',
                relativePath: 'notes/readme.txt',
                size: 80,
                transferredBytes: 80,
                progress: 1,
                speed: 0,
                state: 'completed',
              },
            ],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const t1 = screen.getByTestId('folder-transfer-item-t1');
      fireEvent.click(within(t1).getByText('📁 f1'));

      expect(within(t1).getByText('Files: 2 / 5')).toBeInTheDocument();
      expect(within(t1).getByTestId('folder-file-list')).toBeInTheDocument();
      expect(within(t1).getByText('docs/hello.txt')).toBeInTheDocument();
      expect(within(t1).getByText('50 B / 100 B')).toBeInTheDocument();
      expect(within(t1).getAllByText('10 B/s')).toHaveLength(2);
      expect(invoke).not.toHaveBeenCalled();
    });

    it('falls back to the current file label when linked files are unavailable', () => {
      useFolderTransferStore.setState({
        folderTransfers: [
          {
            id: 't1',
            folderName: 'f1',
            totalSize: 100,
            totalFiles: 5,
            transferredBytes: 10,
            transferredFiles: 2,
            speed: 10,
            state: 'active',
            currentFile: 'hello.txt',
            files: [],
          },
        ],
      });

      render(<FolderTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle folder transfer panel/i }));

      const t1 = screen.getByTestId('folder-transfer-item-t1');
      fireEvent.click(within(t1).getByText('📁 f1'));

      expect(within(t1).getByText('Files: 2 / 5')).toBeInTheDocument();
      expect(within(t1).getByText('Current: hello.txt')).toBeInTheDocument();
    });
  });
});
