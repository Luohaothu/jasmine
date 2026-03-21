import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { MentionAutocomplete } from "./MentionAutocomplete";

const mockPeers = [
  { id: "uuid-1", name: "Alice", status: "online" as const },
  { id: "uuid-2", name: "Bob", status: "offline" as const },
  { id: "uuid-3", name: "Charlie", status: "online" as const },
];

describe("MentionAutocomplete", () => {
  it("renders empty state when no matches", () => {
    render(
      <MentionAutocomplete
        query="xyz"
        peers={mockPeers}
        selectedIndex={0}
        onSelect={vi.fn()}
      />
    );
    expect(screen.getByText("No matches found")).toBeInTheDocument();
  });

  it("filters peers based on query and sorts online first", () => {
    render(
      <MentionAutocomplete
        query=""
        peers={mockPeers}
        selectedIndex={0}
        onSelect={vi.fn()}
      />
    );
    const names = screen.getAllByText(/Alice|Bob|Charlie/).map((el) => el.textContent);
    expect(names).toEqual(["Alice", "Charlie", "Bob"]);
  });

  it("filters by query correctly", () => {
    render(
      <MentionAutocomplete
        query="c"
        peers={mockPeers}
        selectedIndex={0}
        onSelect={vi.fn()}
      />
    );
    const names = screen.getAllByText(/Alice|Charlie/).map((el) => el.textContent);
    expect(names).toEqual(["Alice", "Charlie"]);
    expect(screen.queryByText("Bob")).not.toBeInTheDocument();
  });

  it("calls onSelect when an item is clicked", () => {
    const handleSelect = vi.fn();
    render(
      <MentionAutocomplete
        query=""
        peers={mockPeers}
        selectedIndex={0}
        onSelect={handleSelect}
      />
    );
    
    fireEvent.mouseDown(screen.getByText("Bob"));
    expect(handleSelect).toHaveBeenCalledWith(mockPeers[1]);
  });
});
