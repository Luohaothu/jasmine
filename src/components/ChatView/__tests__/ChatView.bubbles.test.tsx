import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { ChatView } from "../ChatView";
import { useChatStore, ChatMessage } from "../../../stores/chatStore";
import { usePeerStore } from "../../../stores/peerStore";
import messageStyles from "../MessageBubble.module.css";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

window.HTMLElement.prototype.scrollIntoView = vi.fn();

describe("ChatView.bubbles", () => {
  beforeEach(() => {
    useChatStore.setState({ messages: {} });
    usePeerStore.setState({ peers: [{ id: "peer-1", name: "Alice", status: "online" }] });
  });

  const renderWithRouter = (peerId: string) => {
    return render(
      <MemoryRouter initialEntries={[`/chat/${peerId}`]}>
        <Routes>
          <Route path="/chat/:peerId" element={<ChatView />} />
        </Routes>
      </MemoryRouter>
    );
  };

  it("renders a list of messages correctly and applies appropriate own/peer styling classes", () => {
    const messages: ChatMessage[] = [
      { id: "1", senderId: "local", receiverId: "peer-1", content: "Hello", timestamp: 1000, status: "sent" },
      { id: "2", senderId: "peer-1", receiverId: "local", content: "Hi there!", timestamp: 2000, status: "delivered" },
    ];
    useChatStore.getState().setMessages("peer-1", messages);
    
    renderWithRouter("peer-1");
    
    const ownMsg = screen.getByText("Hello").closest(`.${messageStyles.bubble}`);
    expect(ownMsg).toHaveClass(messageStyles.ownBubble);
    expect(ownMsg?.parentElement).toHaveClass(messageStyles.own);

    const peerMsg = screen.getByText("Hi there!").closest(`.${messageStyles.bubble}`);
    expect(peerMsg).toHaveClass(messageStyles.peerBubble);
    expect(peerMsg?.parentElement).toHaveClass(messageStyles.peer);
  });
});
