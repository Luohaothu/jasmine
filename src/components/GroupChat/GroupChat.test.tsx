import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { GroupChat } from "./GroupChat";

// Mocking the router params
vi.mock("react-router-dom", () => ({
  useParams: () => ({ groupId: "g1" }),
}));

// We'll pass group state as props to avoid mocking full store initially if possible, or just mock stores.
const mockMessages = [
  {
    id: "m1",
    senderId: "p1",
    senderName: "Alice", // Group messages need this for display
    content: "Hello group!",
    timestamp: Date.now(),
    isOwn: false,
  },
  {
    id: "m2",
    senderId: "me",
    senderName: "Me",
    content: "Hi Alice!",
    timestamp: Date.now(),
    isOwn: true,
  }
];

const mockMembers = [
  { id: "p1", name: "Alice", status: "online" as const },
  { id: "p2", name: "Bob", status: "offline" as const },
  { id: "me", name: "Me", status: "online" as const },
];

describe("GroupChat", () => {
  it("renders header with group name, member count, and toggle", () => {
    render(<GroupChat groupName="Dev Team" members={mockMembers} messages={mockMessages} onSendMessage={vi.fn()} />);
    
    expect(screen.getByText("Dev Team")).toBeInTheDocument();
    expect(screen.getByText("3 members")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /管理|Manage/i })).toBeInTheDocument();
  });

  it("toggles member panel when manage button is clicked", () => {
    render(<GroupChat groupName="Dev Team" members={mockMembers} messages={mockMessages} onSendMessage={vi.fn()} />);
    
    // Initially panel might be closed (or open depending on default)
    // Let's assume default is closed
    expect(screen.queryByTestId("member-panel")).not.toBeInTheDocument();
    
    // Click Manage
    fireEvent.click(screen.getByRole("button", { name: /管理|Manage/i }));
    
    // Panel should open
    expect(screen.getByTestId("member-panel")).toBeInTheDocument();
    expect(screen.getAllByText("Alice").length).toBeGreaterThan(0);
    
    // Click Manage again
    fireEvent.click(screen.getByRole("button", { name: /管理|Manage/i }));
    
    // Panel should close
    expect(screen.queryByTestId("member-panel")).not.toBeInTheDocument();
  });

  it("renders messages and senderName on peer messages but not own", () => {
    render(<GroupChat groupName="Dev Team" members={mockMembers} messages={mockMessages} onSendMessage={vi.fn()} />);
    
    expect(screen.getByText("Hello group!")).toBeInTheDocument();
    expect(screen.getByText("Hi Alice!")).toBeInTheDocument();
    
    // "Alice" should be rendered as senderName
    expect(screen.getAllByText("Alice").length).toBeGreaterThan(0);
    
    // "Me" should probably not be rendered as a senderName for own message (or we can assert it based on the class)
    // There might be a "Me" in the member list if panel is open, but panel is closed by default.
    // If it's closed, "Me" shouldn't be rendered anywhere unless there's an issue.
    expect(screen.queryByText("Me")).not.toBeInTheDocument();
  });
});
