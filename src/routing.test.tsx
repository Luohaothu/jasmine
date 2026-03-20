import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { RouterProvider, createMemoryRouter } from "react-router-dom";
import { appRouter } from "./router";

describe("Routing and Placeholders", () => {
  it("renders the welcome placeholder on the root route", () => {
    const router = createMemoryRouter(appRouter.routes, { initialEntries: ["/"] });
    render(<RouterProvider router={router} />);
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    expect(screen.getByText(/Select a chat to start messaging/i)).toBeInTheDocument();
  });

  it("renders the /chat/:peerId view with persistent sidebar", () => {
    window.HTMLElement.prototype.scrollIntoView = vi.fn();
    const router = createMemoryRouter(appRouter.routes, { initialEntries: ["/chat/peer-123"] });
    render(<RouterProvider router={router} />);
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    expect(screen.getByText(/发送第一条消息开始聊天/i)).toBeInTheDocument();
  });

  it("renders the /group/:groupId placeholder with persistent sidebar", () => {
    const router = createMemoryRouter(appRouter.routes, { initialEntries: ["/group/group-456"] });
    render(<RouterProvider router={router} />);
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    expect(screen.getByText(/Group not found/i)).toBeInTheDocument();
  });

  it("renders the /settings placeholder with persistent sidebar", () => {
    const router = createMemoryRouter(appRouter.routes, { initialEntries: ["/settings"] });
    render(<RouterProvider router={router} />);
    expect(screen.getByTestId("sidebar")).toBeInTheDocument();
    expect(screen.getAllByText(/Settings/i)[0]).toBeInTheDocument();
  });
});
