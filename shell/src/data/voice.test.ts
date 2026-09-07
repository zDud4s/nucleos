import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { postSegment } from "./voice";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("postSegment", () => {
  it("maps an empty 204 response to a continuing segment", async () => {
    daemon.apiFetch.mockResolvedValue(undefined);

    await expect(postSegment(new Uint8Array([1]), 900)).resolves.toEqual({
      text: "",
      verdict: "continues",
    });
  });
});
