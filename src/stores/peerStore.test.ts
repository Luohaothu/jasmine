import { describe, it, expect } from "vitest";
import { usePeerStore } from "./peerStore";

describe("usePeerStore", () => {
  it("initializes with empty peers", () => {
    const peers = usePeerStore.getState().peers;
    expect(peers).toEqual([]);
  });
});
