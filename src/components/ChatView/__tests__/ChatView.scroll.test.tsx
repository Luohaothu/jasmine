import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import { render, act } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { ChatView } from "../ChatView";
import { useChatStore } from "../../../stores/chatStore";
import { usePeerStore } from "../../../stores/peerStore";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

describe("ChatView.scroll", () => {
  beforeEach(() => {
    useChatStore.setState({ messages: {} });
    usePeerStore.setState({ peers: [{ id: "peer-1", name: "Alice", status: "online" }] });
  });

  afterEach(() => {
    vi.restoreAllMocks();
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

  it("auto-scrolls to the newest message when messages change", () => {
    const mockScrollIntoView = vi.fn();
    window.HTMLElement.prototype.scrollIntoView = mockScrollIntoView;

    renderWithRouter("peer-1");

    act(() => {
      useChatStore.getState().addMessage("peer-1", {
        id: "msg-1",
        senderId: "local",
        receiverId: "peer-1",
        content: "New Message",
        timestamp: 1000,
        status: "sent",
      });
    });

    expect(mockScrollIntoView).toHaveBeenCalled();
  });
});
