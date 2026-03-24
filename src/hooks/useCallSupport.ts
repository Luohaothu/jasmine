import { useEffect, useState } from 'react';

import { invoke } from '@tauri-apps/api/core';

export interface CallSupportInfo {
  supported: boolean;
  platform: string;
  reason?: string;
}

export interface CallSupportState extends CallSupportInfo {
  loading: boolean;
  error?: string;
}

type CallSupportListener = () => void;

let cachedCallSupport: CallSupportInfo | null = null;
let loadError: string | null = null;
let loadPromise: Promise<void> | null = null;
const callSupportListeners = new Set<CallSupportListener>();

function normalizeError(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }

  return String(error);
}

function notifyListeners(): void {
  for (const listener of callSupportListeners) {
    listener();
  }
}

function snapshotState(): CallSupportState {
  if (cachedCallSupport) {
    return {
      loading: false,
      supported: cachedCallSupport.supported,
      platform: cachedCallSupport.platform,
      reason: cachedCallSupport.reason,
    };
  }

  if (loadPromise) {
    return {
      loading: true,
      supported: false,
      platform: '',
      error: loadError ?? undefined,
    };
  }

  return {
    loading: false,
    supported: false,
    platform: '',
    error: loadError ?? undefined,
  };
}

function loadCallSupport(): void {
  if (cachedCallSupport || loadPromise || loadError) {
    return;
  }

  loadPromise = invoke<CallSupportInfo>('check_call_support')
    .then((info) => {
      cachedCallSupport = info;
    })
    .catch((error) => {
      loadError = normalizeError(error);
    })
    .finally(() => {
      loadPromise = null;
      notifyListeners();
    });

  notifyListeners();
}

export function clearCallSupportCacheForTest(): void {
  cachedCallSupport = null;
  loadError = null;
  loadPromise = null;
  callSupportListeners.clear();
  notifyListeners();
}

export function useCallSupport(): CallSupportState {
  const [state, setState] = useState<CallSupportState>(snapshotState);

  useEffect(() => {
    const update = (): void => {
      setState(snapshotState());
    };

    loadCallSupport();
    callSupportListeners.add(update);
    update();

    return () => {
      callSupportListeners.delete(update);
    };
  }, []);

  return state;
}
