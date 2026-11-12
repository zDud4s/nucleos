import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { ApiRefusal } from "./client";
import {
  classifyWriteFailure,
  settingPath,
  useSettingsIndex,
  useWriteSetting,
} from "./settings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("settingPath", () => {
  it("encodes both segments, so a project scope keeps its colon out of the path", () => {
    expect(settingPath("machine", "email")).toBe("/settings/machine/email");
    expect(settingPath("project:alpha", "autopilot")).toBe("/settings/project%3Aalpha/autopilot");
  });

  it("keeps a channel's name as one encoded segment", () => {
    expect(settingPath("machine", "channels/telegram")).toBe("/settings/machine/channels%2Ftelegram");
  });
});

/** The refusal `client.ts` builds from the núcleo's body: the name is the code, and `detail` wins as the sentence. */
function refusalFrom(status: number, body: { refusal: string; detail?: string; [key: string]: unknown }): ApiRefusal {
  return new ApiRefusal(status, body.refusal, body.detail ?? body.refusal, body);
}

describe("classifyWriteFailure", () => {
  it("sorts a refusal by status, not by its words", () => {
    expect(classifyWriteFailure(refusalFrom(409, { refusal: "stale", current: 4 }))).toEqual({
      kind: "stale",
    });
    expect(classifyWriteFailure(refusalFrom(423, { refusal: "kill_switch" }))).toEqual({
      kind: "kill_switch",
    });
    expect(classifyWriteFailure(refusalFrom(403, { refusal: "not_ours" }))).toEqual({
      kind: "not_ours",
    });
  });

  it("reads the field of a 422 from the body when it names one", () => {
    const refusal = refusalFrom(422, { refusal: "invalid", field: "/hotkey", detail: "expected a chord" });
    expect(classifyWriteFailure(refusal)).toEqual({
      kind: "invalid",
      field: "/hotkey",
      message: "expected a chord",
    });
  });

  it("keeps a 422 without a field as a message alone", () => {
    const refusal = refusalFrom(422, { refusal: "invalid", detail: "no good" });
    expect(classifyWriteFailure(refusal)).toEqual({ kind: "invalid", field: null, message: "no good" });
    const bare = new ApiRefusal(422, "invalid", "the parser said no");
    expect(classifyWriteFailure(bare)).toEqual({
      kind: "invalid",
      field: null,
      message: "the parser said no",
    });
  });

  it("takes a 422's words from detail, because the wire has no message", () => {
    const refusal = refusalFrom(422, { refusal: "invalid", detail: "from the detail", message: "not on the wire" });
    expect(classifyWriteFailure(refusal)).toEqual({ kind: "invalid", field: null, message: "from the detail" });
  });

  it("treats any other refusal as a refusal and a non-refusal as unreachable", () => {
    expect(classifyWriteFailure(refusalFrom(500, { refusal: "internal" }))).toEqual({
      kind: "refused",
      message: "internal",
    });
    expect(classifyWriteFailure(refusalFrom(400, { refusal: "bad_scope", detail: "no such scope" }))).toEqual({
      kind: "refused",
      message: "no such scope",
    });
    expect(classifyWriteFailure(new Error("network"))).toEqual({ kind: "unreachable" });
  });
});

function IndexProbe({ scope }: { scope?: string }) {
  useSettingsIndex(scope);
  return null;
}

function WriteProbe() {
  const write = useWriteSetting("project:alpha", "autopilot");
  return (
    <button type="button" onClick={() => { write.mutate({ body: { a: 1 }, version: 2 }); }}>
      go
    </button>
  );
}

describe("the hooks", () => {
  it("asks for the whole index with no query, and for a scope with one", async () => {
    daemon.apiFetch.mockResolvedValue([]);
    renderWithQuery(<IndexProbe />);
    await waitFor(() => { expect(daemon.apiFetch).toHaveBeenCalledWith("/settings"); });
    renderWithQuery(<IndexProbe scope="project:alpha" />);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/settings?scope=project%3Aalpha");
    });
  });

  it("writes the body together with the version it was read at", async () => {
    daemon.apiFetch.mockResolvedValue({ version: 3 });
    renderWithQuery(<WriteProbe />);
    fireEvent.click(screen.getByText("go"));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/settings/project%3Aalpha/autopilot", {
        method: "PUT",
        body: JSON.stringify({ body: { a: 1 }, version: 2 }),
      });
    });
  });
});
