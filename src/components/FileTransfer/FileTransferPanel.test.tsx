import { render, screen, fireEvent, within } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { FileTransferPanel } from './FileTransferPanel';
import { useTransferStore } from '../../stores/transferStore';
import { invoke } from '@tauri-apps/api/core';
import '../../i18n/i18n';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

describe('FileTransferPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useTransferStore.setState({
      transfers: [],
      activeOffers: [],
    });
  });

  it('does not render when no transfers', () => {
    const { container } = render(<FileTransferPanel />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders fab and badge correctly', () => {
    useTransferStore.setState({
      transfers: [
        { id: '1', filename: 'active.txt', size: 100, progress: 0.5, speed: 10, state: 'active' },
        { id: '2', filename: 'queued.txt', size: 200, progress: 0, speed: 0, state: 'queued' },
        { id: '3', filename: 'done.txt', size: 300, progress: 1, speed: 0, state: 'completed' },
      ],
    });

    render(<FileTransferPanel />);
    const fab = screen.getByRole('button', { name: /Toggle transfer panel/i });
    expect(fab).toBeInTheDocument();

    const badge = screen.getByTestId('active-badge');
    expect(badge).toHaveTextContent('2');
  });

  it('toggles panel visibility', () => {
    useTransferStore.setState({
      transfers: [{ id: '1', filename: 'f', size: 10, progress: 0.1, speed: 5, state: 'active' }],
    });

    render(<FileTransferPanel />);
    expect(screen.queryByTestId('transfer-panel')).not.toBeInTheDocument();

    const fab = screen.getByRole('button', { name: /Toggle transfer panel/i });
    fireEvent.click(fab);

    expect(screen.getByTestId('transfer-panel')).toBeInTheDocument();

    const closeBtn = screen.getByRole('button', { name: /Minimize panel/i });
    fireEvent.click(closeBtn);

    expect(screen.queryByTestId('transfer-panel')).not.toBeInTheDocument();
  });

  describe('FileTransferPanel.progress', () => {
    it('displays correct progress, speed, and status for different states', () => {
      useTransferStore.setState({
        transfers: [
          { id: 't1', filename: 'f1', size: 1024, progress: 0.5, speed: 512, state: 'active' },
          { id: 't2', filename: 'f2', size: 2048, progress: 1, speed: 0, state: 'completed' },
        ],
      });

      render(<FileTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle transfer panel/i }));

      const t1 = screen.getByTestId('transfer-item-t1');
      expect(within(t1).getByText('active')).toBeInTheDocument();
      expect(within(t1).getByText('1 KB')).toBeInTheDocument();
      expect(within(t1).getByText('512 B/s')).toBeInTheDocument();

      const t2 = screen.getByTestId('transfer-item-t2');
      expect(within(t2).getByText('completed')).toBeInTheDocument();
      expect(within(t2).getByText('2 KB')).toBeInTheDocument();
      expect(within(t2).queryByText('B/s')).not.toBeInTheDocument();
    });
  });

  describe('FileTransferPanel.cancel', () => {
    it('calls invoke with cancel_transfer on cancel button click', async () => {
      useTransferStore.setState({
        transfers: [
          { id: 't1', filename: 'f1', size: 100, progress: 0.1, speed: 10, state: 'active' },
        ],
      });

      render(<FileTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle transfer panel/i }));

      const cancelBtn = screen.getByRole('button', { name: /Cancel transfer/i });
      fireEvent.click(cancelBtn);

      expect(invoke).toHaveBeenCalledWith('cancel_transfer', { transfer_id: 't1' });
    });
  });

  describe('FileTransferPanel.resumeRetry', () => {
    it('shows Resume and Retry for failed resumable transfers and invokes both actions', async () => {
      useTransferStore.setState({
        transfers: [
          {
            id: 't-resume',
            filename: 'resume.bin',
            size: 100,
            progress: 0.4,
            speed: 0,
            state: 'failed',
            resumable: true,
            bytes_transferred: 40,
          },
        ],
      });

      render(<FileTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle transfer panel/i }));

      const item = screen.getByTestId('transfer-item-t-resume');

      const resumeBtn = within(item).getByRole('button', { name: /Resume transfer/i });
      fireEvent.click(resumeBtn);
      expect(invoke).toHaveBeenCalledWith('resume_transfer', { transferId: 't-resume' });
    });

    it('shows Retry and invokes retry action', async () => {
      useTransferStore.setState({
        transfers: [
          {
            id: 't-retry',
            filename: 'retry.bin',
            size: 100,
            progress: 0.4,
            speed: 0,
            state: 'failed',
            resumable: true,
            bytes_transferred: 40,
          },
        ],
      });

      render(<FileTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle transfer panel/i }));

      const item = screen.getByTestId('transfer-item-t-retry');
      const retryBtn = within(item).getByRole('button', { name: /Retry transfer/i });
      fireEvent.click(retryBtn);
      expect(invoke).toHaveBeenCalledWith('retry_transfer', { transferId: 't-retry' });
    });

    it('shows Retry only for failed non-resumable transfers and keeps active cancel behavior', () => {
      useTransferStore.setState({
        transfers: [
          {
            id: 't-failed',
            filename: 'failed.bin',
            size: 100,
            progress: 0.1,
            speed: 0,
            state: 'failed',
            resumable: false,
          },
          {
            id: 't-active',
            filename: 'active.bin',
            size: 100,
            progress: 0.5,
            speed: 10,
            state: 'active',
          },
        ],
      });

      render(<FileTransferPanel />);
      fireEvent.click(screen.getByRole('button', { name: /Toggle transfer panel/i }));

      const failedItem = screen.getByTestId('transfer-item-t-failed');
      expect(
        within(failedItem).queryByRole('button', { name: /Resume transfer/i })
      ).not.toBeInTheDocument();
      expect(
        within(failedItem).getByRole('button', { name: /Retry transfer/i })
      ).toBeInTheDocument();
      expect(
        within(failedItem).queryByRole('button', { name: /Cancel transfer/i })
      ).not.toBeInTheDocument();

      const activeItem = screen.getByTestId('transfer-item-t-active');
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
});
