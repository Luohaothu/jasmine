import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { MessageBubble } from "../MessageBubble";
import styles from "../MessageBubble.module.css";

describe("MessageBubble", () => {
  const testDate = new Date();
  testDate.setHours(14, 5, 0, 0);
  const timestamp = testDate.getTime();

  it("renders own message with sent status, proper alignment, blue styling, and exact HH:mm", () => {
    const { container } = render(
      <MessageBubble
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="sent"
      />
    );
    expect(screen.getByText("Hello there")).toBeInTheDocument();
    
    expect(screen.getByText("14:05")).toBeInTheDocument();
    
    expect(screen.getByText("✓")).toBeInTheDocument();

    const wrapper = container.firstElementChild;
    expect(wrapper).toHaveClass(styles.own);

    const bubble = wrapper?.querySelector(`.${styles.bubble}`);
    expect(bubble).toHaveClass(styles.ownBubble);
  });

  it("renders own message with delivered status", () => {
    render(
      <MessageBubble
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="delivered"
      />
    );
    expect(screen.getByText("✓✓")).toBeInTheDocument();
  });

  it("renders own message with failed status", () => {
    render(
      <MessageBubble
        content="Hello there"
        timestamp={timestamp}
        isOwn={true}
        status="failed"
      />
    );
    expect(screen.getByText("❌")).toBeInTheDocument();
  });

  it("renders peer message with proper alignment and gray styling without status icons", () => {
    const { container } = render(
      <MessageBubble
        content="Peer message"
        timestamp={timestamp}
        isOwn={false}
      />
    );
    expect(screen.getByText("Peer message")).toBeInTheDocument();
    expect(screen.queryByText("✓")).not.toBeInTheDocument();
    expect(screen.queryByText("✓✓")).not.toBeInTheDocument();
    expect(screen.queryByText("❌")).not.toBeInTheDocument();

    const wrapper = container.firstElementChild;
    expect(wrapper).toHaveClass(styles.peer);

    const bubble = wrapper?.querySelector(`.${styles.bubble}`);
    expect(bubble).toHaveClass(styles.peerBubble);
  });
});
