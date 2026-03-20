import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { MemberPanel } from "./MemberPanel";
import { Peer } from "../../types/peer";

const mockMembers: Peer[] = [
  { id: "p1", name: "Alice", status: "online" },
  { id: "p2", name: "Bob", status: "offline" },
];

describe("MemberPanel", () => {
  it("renders the list of members and their statuses", () => {
    render(<MemberPanel isOpen={true} members={mockMembers} />);
    
    expect(screen.getByText("Members (2)")).toBeInTheDocument();
    expect(screen.getByText("Alice")).toBeInTheDocument();
    expect(screen.getByText("Bob")).toBeInTheDocument();
    
    const onlineIndicators = screen.getAllByTestId("status-indicator");
    expect(onlineIndicators[0]).toHaveAttribute("aria-label", "online");
    expect(onlineIndicators[1]).toHaveAttribute("aria-label", "offline");
  });

  it("does not render when isOpen is false", () => {
    const { container } = render(<MemberPanel isOpen={false} members={mockMembers} />);
    expect(container.firstChild).toBeNull();
  });
});
