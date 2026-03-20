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
});
