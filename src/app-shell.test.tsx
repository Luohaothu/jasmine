import { describe, expect, it, beforeAll } from "vitest";
import { render, screen } from "@testing-library/react";
import fs from "fs";
import path from "path";

import { RouterProvider, createMemoryRouter } from "react-router-dom";
import { appRouter } from "./router";

describe("App Shell and Theme", () => {
  let cssContent = "";

  beforeAll(() => {
    // Read the CSS file directly for variable validation
    const cssPath = path.resolve(__dirname, "./styles/theme.css");
    cssContent = fs.readFileSync(cssPath, "utf-8");

    // Inject CSS into the document head so getComputedStyle works for layout
    const style = document.createElement("style");
    style.innerHTML = cssContent;
    document.head.appendChild(style);
  });

  it("renders a 280px sidebar that is always visible", () => {
    const router = createMemoryRouter(appRouter.routes, { initialEntries: ["/"] });
    render(<RouterProvider router={router} />);
    const sidebar = screen.getByTestId("sidebar");
    expect(sidebar).toBeInTheDocument();
    
    const computedStyle = window.getComputedStyle(sidebar);
    expect(computedStyle.width).toBe("280px");
    expect(computedStyle.minWidth).toBe("280px");
    expect(computedStyle.maxWidth).toBe("280px");
  });

  it("applies light theme variables via CSS custom properties", () => {
    // We expect the default :root to contain the light mode variables
    const lightModeMatch = cssContent.match(/:root\s*{([^}]+)}/);
    expect(lightModeMatch).not.toBeNull();
    const lightVars = lightModeMatch![1];
    
    expect(lightVars).toContain("--bg-primary: #ffffff");
    expect(lightVars).toContain("--bg-secondary: #f4f4f5");
    expect(lightVars).toContain("--text-primary: #18181b");
  });

  it("applies dark theme variables via CSS custom properties when prefers-color-scheme is dark", () => {
    // Find the media query block
    const mediaQueryMatch = cssContent.match(/@media\s*\(prefers-color-scheme:\s*dark\)\s*{[\s\S]*?:root\s*{([^}]+)}/);
    expect(mediaQueryMatch).not.toBeNull();
    const darkVars = mediaQueryMatch![1];
    
    expect(darkVars).toContain("--bg-primary: #18181b");
    expect(darkVars).toContain("--bg-secondary: #27272a");
    expect(darkVars).toContain("--text-primary: #f4f4f5");
  });
});
