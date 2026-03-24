import { beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
}));

import { clearCallSupportCacheForTest, useCallSupport } from './useCallSupport';

describe('useCallSupport', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    clearCallSupportCacheForTest();
  });

  it('loads call support once and shares cached state across hook consumers', async () => {
    const result = {
      supported: false,
      platform: 'linux',
      reason: 'Linux calling support is not enabled yet',
    };

    invokeMock.mockResolvedValueOnce(result);
    const first = renderHook(() => useCallSupport());
    const second = renderHook(() => useCallSupport());

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledTimes(1);
    });

    await waitFor(() => {
      expect(first.result.current.loading).toBe(false);
      expect(first.result.current.supported).toBe(false);
      expect(first.result.current.platform).toBe('linux');
      expect(first.result.current.reason).toBe(result.reason);
    });

    expect(second.result.current).toEqual(first.result.current);
  });

  it('uses loading state while the command is in flight', async () => {
    let resolveInvoke: (value: { supported: boolean; platform: string; reason?: string }) => void;
    invokeMock.mockImplementation(
      () =>
        new Promise<{ supported: boolean; platform: string; reason?: string }>((resolve) => {
          resolveInvoke = resolve;
        })
    );

    const { result } = renderHook(() => useCallSupport());

    expect(result.current.loading).toBe(true);
    expect(result.current.supported).toBe(false);

    resolveInvoke!({ supported: true, platform: 'windows', reason: undefined });

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
      expect(result.current.supported).toBe(true);
      expect(result.current.platform).toBe('windows');
    });
  });
});
