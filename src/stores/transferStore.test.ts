import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useTransferStore } from './transferStore';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

describe('useTransferStore', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    useTransferStore.setState({ transfers: [], activeOffers: [] });
  });

  it('initializes with empty transfers', () => {
    const transfers = useTransferStore.getState().transfers;
    expect(transfers).toEqual([]);
  });

  it('can add messages for a peer', () => {
    const transfer = {
      id: 'transfer-1',
      filename: 'hello.txt',
      size: 42,
      progress: 0.25,
      speed: 10,
      state: 'active' as const,
      resumable: false,
    };
    useTransferStore.getState().addTransfer(transfer);
    expect(useTransferStore.getState().transfers).toEqual([transfer]);
  });

  it('merges resumable metadata for existing transfer entries', () => {
    useTransferStore.getState().addTransfer({
      id: 'transfer-1',
      filename: 'hello.txt',
      size: 42,
      progress: 0.25,
      speed: 10,
      state: 'failed',
      resumable: false,
    });

    useTransferStore.getState().addTransfer({
      id: 'transfer-1',
      filename: 'hello.txt',
      size: 42,
      progress: 0.25,
      speed: 10,
      state: 'failed',
      resumable: true,
      localPath: '/tmp/hello.txt',
      direction: 'receive',
    });

    expect(useTransferStore.getState().transfers).toEqual([
      {
        id: 'transfer-1',
        filename: 'hello.txt',
        size: 42,
        progress: 0.25,
        speed: 10,
        state: 'failed',
        resumable: true,
        localPath: '/tmp/hello.txt',
        direction: 'receive',
      },
    ]);
  });

  it('resumeTransfer calls the backend command with the transfer id', async () => {
    invokeMock.mockResolvedValueOnce('resumed-transfer-1');

    const resumedId = await useTransferStore.getState().resumeTransfer('transfer-1');

    expect(resumedId).toBe('resumed-transfer-1');
    expect(invokeMock).toHaveBeenCalledWith('resume_transfer', { transferId: 'transfer-1' });
  });

  it('retryTransfer calls the backend command with the transfer id', async () => {
    invokeMock.mockResolvedValueOnce('retried-transfer-1');

    const retriedId = await useTransferStore.getState().retryTransfer('transfer-1');

    expect(retriedId).toBe('retried-transfer-1');
    expect(invokeMock).toHaveBeenCalledWith('retry_transfer', { transferId: 'transfer-1' });
  });
});
