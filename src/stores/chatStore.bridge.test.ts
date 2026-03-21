import { beforeEach, describe, expect, it, vi } from "vitest";

import { setupChatEventListeners, useChatStore } from "./chatStore";

const listeners = new Map<string, Function>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (eventName: string, callback: Function) => {
    listeners.set(eventName, callback);
    return () => {
      listeners.delete(eventName);
    };
  }),
}));

describe("chatStore bridge listeners", () => {
  beforeEach(() => {
    listeners.clear();
    useChatStore.setState({
      messages: {
        "peer-1": [
          {
            id: "msg-1",
            senderId: "peer-1",
            receiverId: "local",
            content: "Before",
            timestamp: 1,
            status: "delivered",
            transferId: "transfer-1",
            metadata: {
              originalPath: "/tmp/photo.jpg",
              thumbnailState: "pending",
            },
          },
        ],
      },
      replyingTo: null,
    });
  });

  it("applies edited, deleted, and thumbnail bridge events across stored messages", async () => {
    const cleanup = await setupChatEventListeners();

    const messageEdited = listeners.get("message-edited");
    messageEdited?.({
      payload: {
        messageId: "msg-1",
        newContent: "After",
        editedAt: 99,
        editVersion: 2,
      },
    });

    let message = useChatStore.getState().messages["peer-1"]?.[0];
    expect(message?.content).toBe("After");
    expect(message?.editedAt).toBe(99);
    expect(message?.editVersion).toBe(2);

    const thumbnailReady = listeners.get("thumbnail-ready");
    thumbnailReady?.({
      payload: {
        transferId: "transfer-1",
        thumbnailPath: "/tmp/thumb.webp",
      },
    });

    message = useChatStore.getState().messages["peer-1"]?.[0];
    expect(message?.type).toBe("image");
    expect(message?.metadata?.thumbnailState).toBe("ready");
    expect(message?.metadata?.thumbnailPath).toBe("/tmp/thumb.webp");

    const messageDeleted = listeners.get("message-deleted");
    messageDeleted?.({
      payload: {
        messageId: "msg-1",
      },
    });

    message = useChatStore.getState().messages["peer-1"]?.[0];
    expect(message?.isDeleted).toBe(true);
    expect(message?.content).toBe("");

    const thumbnailFailed = listeners.get("thumbnail-failed");
    thumbnailFailed?.({
      payload: {
        transferId: "transfer-1",
      },
    });

    message = useChatStore.getState().messages["peer-1"]?.[0];
    expect(message?.metadata?.thumbnailState).toBe("failed");

    cleanup();
  });
});
