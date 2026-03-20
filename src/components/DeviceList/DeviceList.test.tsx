import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { BrowserRouter } from "react-router-dom";
import { usePeerStore } from "../../stores/peerStore";
import { DeviceList } from "./DeviceList";

const mockPeers = [
  { id: "peer-1", name: "Alice", status: "online" as const },
  { id: "peer-2", name: "Bob", status: "offline" as const },
  { id: "peer-3", name: "Charlie", status: "online" as const },
];

const renderDeviceList = () => {
  return render(
    <BrowserRouter>
      <DeviceList />
    </BrowserRouter>
  );
};

describe("DeviceList", () => {
  beforeEach(() => {
    usePeerStore.setState({ peers: [] });
  });

  it("renders empty state with spinner when no peers", () => {
    renderDeviceList();
    expect(screen.getByText(/正在搜索局域网设备/i)).toBeInTheDocument();
  });

  it("renders list of peers with avatars and online/offline status indicators", () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();
    
    expect(screen.getByText("Alice")).toBeInTheDocument();
    expect(screen.getByText("Bob")).toBeInTheDocument();
    expect(screen.getByText("Charlie")).toBeInTheDocument();

    expect(screen.getByText("A")).toBeInTheDocument();
    expect(screen.getByText("B")).toBeInTheDocument();
    expect(screen.getByText("C")).toBeInTheDocument();

    const onlineStatuses = screen.getAllByText("online");
    expect(onlineStatuses).toHaveLength(2);
    
    const offlineStatuses = screen.getAllByText("offline");
    expect(offlineStatuses).toHaveLength(1);
  });

  it("filters peers by search", () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    const searchInput = screen.getByPlaceholderText(/Search devices/i);
    fireEvent.change(searchInput, { target: { value: "ali" } });

    expect(screen.getByText("Alice")).toBeInTheDocument();
    expect(screen.queryByText("Bob")).not.toBeInTheDocument();
    expect(screen.queryByText("Charlie")).not.toBeInTheDocument();
  });

  it("navigates to chat when a peer is clicked", () => {
    usePeerStore.setState({ peers: mockPeers });
    renderDeviceList();

    const aliceLink = screen.getByRole("link", { name: /Alice/i });
    expect(aliceLink).toHaveAttribute("href", "/chat/peer-1");
  });
});
