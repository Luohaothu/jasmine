import { describe, it, expect } from "vitest";
import { useTransferStore } from "./transferStore";

describe("useTransferStore", () => {
  it("initializes with empty transfers", () => {
    const transfers = useTransferStore.getState().transfers;
    expect(transfers).toEqual([]);
  });
});
