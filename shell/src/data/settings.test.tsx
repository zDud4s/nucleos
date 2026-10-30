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
});

describe("classifyWriteFailure", () => {
  it("sorts a refusal by status, not by its words", () => {
    expect(classifyWriteFailure(new ApiRefusal(409, "refused", "stale", { error: "stale", current: 4 }))).toEqual({
      kind: "stale",
    });
    expect(classifyWriteFailure(new ApiRefusal(423, "refused", "kill_switch"))).toEqual({
      kind: "kill_switch",
    });
    expect(classifyWriteFailure(new ApiRefusal(403, "refused", "not_ours"))).toEqual({
      kind: "not_ours",
    });
  });

  it("reads the field and the message of a 422 from the body", () => {
    const refusal = new ApiRefusal(422, "refused", "invalid", {
      error: "invalid",
      field: "/hotkey",
      message: "expected a chord",
    });
    expect(classifyWriteFailure(refusal)).toEqual({
      kind: "invalid",
      field: "/hotkey",
      message: "expected a chord",
    });
  });

  it("keeps a 422 without a field as a message alone", () => {
    const refusal = new ApiRefusal(422, "refused", "bad", { error: "invalid", message: "no good" });
    expect(classifyWriteFailure(refusal)).toEqual({ kind: "invalid", field: null, message: "no good" });
    const bare = new ApiRefusal(422, "refused", "the parser said no");
    expect(classifyWriteFailure(bare)).toEqual({
      kind: "invalid",
      field: null,
      message: "the parser said no",
    });
  });

  it("treats any other refusal as a refusal and a non-refusal as unreachable", () => {
    expect(classifyWriteFailure(new ApiRefusal(500, "boom", "it broke"))).toEqual({
      kind: "refused",
      message: "it broke",
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
