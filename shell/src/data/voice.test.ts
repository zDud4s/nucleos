import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { postDraft, postSegment } from "./voice";

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

describe("postDraft", () => {
  /* A draft of a sentence still being spoken is asked for every few hundred milliseconds, and most
     of the early ones land on a pause. `undefined` there is the daemon saying "nothing yet", which
     must read as an empty revision — `revise` takes its own words back for one — and never as a
     failure that would put a sentence about the microphone in front of somebody mid-dictation. */
  it("maps an empty 204 response to a revision that says nothing", async () => {
    daemon.apiFetch.mockResolvedValue(undefined);

    await expect(postDraft(new Uint8Array([1]), 900)).resolves.toBe("");
  });

  it("asks for the draft kind and never writes a row", async () => {
    daemon.apiFetch.mockResolvedValue({ text: "câmbio" });

    await expect(postDraft(new Uint8Array([1]), 1500)).resolves.toBe("câmbio");
    const [route] = daemon.apiFetch.mock.calls[0];
    expect(route).toContain("kind=draft");
    expect(route).toContain("duration_ms=1500");
  });
});
