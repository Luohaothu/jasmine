import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { ChatHeader } from "../ChatHeader";

describe("ChatHeader", () => {
  it("renders peer name and status", () => {
    render(<ChatHeader peerName="Alice" status="online" />);
    expect(screen.getByText("Alice")).toBeInTheDocument();
    expect(screen.getByText("online")).toBeInTheDocument();
  });

  it("renders offline status correctly", () => {
    render(<ChatHeader peerName="Bob" status="offline" />);
    expect(screen.getByText("Bob")).toBeInTheDocument();
    expect(screen.getByText("offline")).toBeInTheDocument();
  });

  it("renders back button when onBack is provided", () => {
    let clicked = false;
    render(
      <ChatHeader
        peerName="Charlie"
        status="online"
        onBack={() => {
          clicked = true;
        }}
      />
    );
    const backBtn = screen.getByRole("button", { name: /back/i });
    backBtn.click();
    expect(clicked).toBe(true);
  });
});
