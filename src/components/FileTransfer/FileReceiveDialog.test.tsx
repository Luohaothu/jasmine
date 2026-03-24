import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { FileReceiveDialog } from './FileReceiveDialog';
import { useTransferStore } from '../../stores/transferStore';
import '../../i18n/i18n';

import { invoke } from '@tauri-apps/api/core';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

describe('FileReceiveDialog', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useTransferStore.setState({
      transfers: [],
      activeOffers: [{ id: 'o1', filename: 'test.png', size: 1024, senderId: 'user1' }],
    });
  });

  it('renders when there is an active offer', () => {
    render(<FileReceiveDialog />);
    expect(screen.getByText('File Offer Received')).toBeInTheDocument();
    expect(screen.getByText('test.png')).toBeInTheDocument();
    expect(screen.getByText(/1 KB/)).toBeInTheDocument();
  });

  it('calls invoke accept_file and removes offer on Accept', async () => {
    render(<FileReceiveDialog />);
    const acceptBtn = screen.getByRole('button', { name: /Accept/i });
    fireEvent.click(acceptBtn);

    expect(invoke).toHaveBeenCalledWith('accept_file', { offer_id: 'o1' });
    await waitFor(() => {
      expect(useTransferStore.getState().activeOffers).toHaveLength(0);
    });
  });

  it('calls invoke reject_file and removes offer on Reject', async () => {
    render(<FileReceiveDialog />);
    const rejectBtn = screen.getByRole('button', { name: /Reject/i });
    fireEvent.click(rejectBtn);

    expect(invoke).toHaveBeenCalledWith('reject_file', { offer_id: 'o1' });
    await waitFor(() => {
      expect(useTransferStore.getState().activeOffers).toHaveLength(0);
    });
  });

  it('does not render when no offers', () => {
    useTransferStore.setState({ activeOffers: [] });
    const { container } = render(<FileReceiveDialog />);
    expect(container).toBeEmptyDOMElement();
  });
});
