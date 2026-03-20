import { mockIPC } from "@tauri-apps/api/mocks";
import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => {
  cleanup();
});

mockIPC((command) => {
  if (command === "greet") {
    return "Hello";
  }

  return null;
});
