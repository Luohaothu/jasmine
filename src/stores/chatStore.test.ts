import { describe, it, expect } from "vitest";
import { useChatStore } from "./chatStore";

describe("useChatStore", () => {
  it("initializes with empty messages", () => {
    useChatStore.setState({ messages: {} });
    const messages = useChatStore.getState().messages;
    expect(messages).toEqual({});
  });

  it("can add messages for a peer", () => {
    useChatStore.setState({ messages: {} });
    const peerId = "peer-1";
    const message = {
      id: "msg-1",
      senderId: "local",
      receiverId: peerId,
      content: "Hello",
      timestamp: 1234567890,
      status: "sent" as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    expect(useChatStore.getState().messages[peerId]).toEqual([message]);
  });

  it("can edit a message", () => {
    useChatStore.setState({ messages: {} });
    const peerId = "peer-1";
    const message = {
      id: "msg-1",
      senderId: "local",
      receiverId: peerId,
      content: "Hello",
      timestamp: 1234567890,
      status: "sent" as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    useChatStore.getState().editMessage(peerId, "msg-1", "Edited hello");
    
    const messages = useChatStore.getState().messages[peerId];
    expect(messages[0].content).toBe("Edited hello");
    expect(messages[0].editedAt).toBeDefined();
  });

  it("can delete a message (tombstone)", () => {
    useChatStore.setState({ messages: {} });
    const peerId = "peer-1";
    const message = {
      id: "msg-1",
      senderId: "local",
      receiverId: peerId,
      content: "Hello",
      timestamp: 1234567890,
      status: "sent" as const,
    };
    useChatStore.getState().addMessage(peerId, message);
    useChatStore.getState().deleteMessage(peerId, "msg-1");
    
    const messages = useChatStore.getState().messages[peerId];
    expect(messages[0].content).toBe("");
    expect(messages[0].isDeleted).toBe(true);
  });
});
