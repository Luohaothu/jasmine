import { waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { setupFolderTransferListeners, useFolderTransferStore } from "./folderTransferStore";

const listeners = new Map<string, Function>();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (eventName: string, callback: Function) => {
    listeners.set(eventName, callback);
    return () => {
      listeners.delete(eventName);
    };
  }),
}));

describe("useFolderTransferStore", () => {
  beforeEach(() => {
    listeners.clear();
    vi.clearAllMocks();
    vi.mocked(invoke).mockResolvedValue([]);
    useFolderTransferStore.setState({
      folderTransfers: [],
      activeFolderOffers: [],
    });
  });

  it("initializes with empty transfers", () => {
    const transfers = useFolderTransferStore.getState().folderTransfers;
    expect(transfers).toEqual([]);
  });

  it("maps canonical folder bridge events into offer, progress, completion, and linked files", async () => {
    vi.mocked(invoke).mockResolvedValue([
      {
        id: "file-2",
        filename: "icon.bin",
        size: 2048,
        progress: 1,
        speed: 0,
        state: "completed",
        folderId: "folder-1",
        folderRelativePath: "images/icon.bin",
      },
      {
        id: "file-1",
        filename: "report.txt",
        size: 1024,
        progress: 0.5,
        speed: 256,
        state: "active",
        folderId: "folder-1",
        folderRelativePath: "docs/report.txt",
      },
      {
        id: "standalone",
        filename: "notes.txt",
        size: 128,
        progress: 0.5,
        speed: 64,
        state: "active",
      },
    ]);

    const cleanup = await setupFolderTransferListeners();

    const folderOfferReceived = listeners.get("folder-offer-received");
    folderOfferReceived?.({
      payload: {
        folderTransferId: "folder-1",
        folderName: "photos",
        fileCount: 3,
        totalSize: 4096,
        senderName: "Alice",
      },
    });

    expect(useFolderTransferStore.getState().activeFolderOffers).toEqual([
      {
        id: "folder-1",
        folderName: "photos",
        totalSize: 4096,
        totalFiles: 3,
        senderName: "Alice",
      },
    ]);

    useFolderTransferStore.getState().addFolderTransfer({
      id: "folder-1",
      folderName: "photos",
      totalSize: 4096,
      totalFiles: 3,
      transferredBytes: 0,
      transferredFiles: 0,
      speed: 0,
      state: "queued",
      senderName: "Alice",
      files: [],
    });

    const folderProgress = listeners.get("folder-progress");
    folderProgress?.({
      payload: {
        folderTransferId: "folder-1",
        sentBytes: 2048,
        totalBytes: 4096,
        completedFiles: 1,
        totalFiles: 3,
      },
    });

    await waitFor(() => {
      expect(useFolderTransferStore.getState().folderTransfers).toEqual([
        {
          id: "folder-1",
          folderName: "photos",
          totalSize: 4096,
          totalFiles: 3,
          transferredBytes: 2048,
          transferredFiles: 1,
          speed: 0,
          state: "active",
          currentFile: "docs/report.txt",
          senderName: "Alice",
          files: [
            {
              id: "file-1",
              filename: "report.txt",
              relativePath: "docs/report.txt",
              size: 1024,
              transferredBytes: 512,
              progress: 0.5,
              speed: 256,
              state: "active",
            },
            {
              id: "file-2",
              filename: "icon.bin",
              relativePath: "images/icon.bin",
              size: 2048,
              transferredBytes: 2048,
              progress: 1,
              speed: 0,
              state: "completed",
            },
          ],
        },
      ]);
    });

    const folderCompleted = listeners.get("folder-completed");
    folderCompleted?.({
      payload: {
        folderTransferId: "folder-1",
        status: "completed",
      },
    });

    await waitFor(() => {
      expect(useFolderTransferStore.getState().folderTransfers[0]?.state).toBe("completed");
      expect(useFolderTransferStore.getState().folderTransfers[0]?.files).toHaveLength(2);
    });

    cleanup();
  });

  it("maps partially-failed status to partially-failed state", async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    const cleanup = await setupFolderTransferListeners();

    useFolderTransferStore.getState().addFolderTransfer({
      id: "folder-3",
      folderName: "Docs",
      totalSize: 500,
      totalFiles: 5,
      transferredBytes: 400,
      transferredFiles: 4,
      speed: 0,
      state: "active",
      files: [],
    });

    const folderCompleted = listeners.get("folder-completed");
    folderCompleted?.({
      payload: {
        folderTransferId: "folder-3",
        status: "partially-failed",
      },
    });

    await waitFor(() => {
      const store = useFolderTransferStore.getState();
      expect(store.folderTransfers.find(t => t.id === "folder-3")?.state).toBe("partially-failed");
    });

    cleanup();
  });
});
