import { describe, expect, it, vi } from "vitest";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { render, screen, waitFor } from "@testing-library/react";
import { UntrustedNotice } from "./UntrustedNotice";
import { createAppQueryClient } from "../app/queryClient";

const NOTICE =
  "This tool reads third-party text: the end of the turn will rotate the session (with summary).";

function project(over: { cwd: string | null; session: string | null }) {
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/assistant/chats/c/project") {
      return { tools: true, permission_mode: "manual", ...over };
    }
    return null;
  });
}

function mount(firstCall: { tool: string } | null | undefined) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <UntrustedNotice chatId="c" firstCall={firstCall} />
    </QueryClientProvider>,
  );
}

describe("UntrustedNotice", () => {
  it("shows the rotate-with-summary card when the transcript carries untrusted_first_call and the chat has a root and a session", async () => {
    project({ cwd: "/r", session: "sess-1" });
    mount({ tool: "get_email" });
    const note = await screen.findByText(NOTICE);
    expect(note).toBeTruthy();
  });

  it("renders nothing without a root", async () => {
    project({ cwd: null, session: "sess-1" });
    mount({ tool: "get_email" });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    expect(screen.queryByText(NOTICE)).toBeNull();
  });

  it("renders nothing without a live session", async () => {
    project({ cwd: "/r", session: null });
    mount({ tool: "get_email" });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    expect(screen.queryByText(NOTICE)).toBeNull();
  });

  it("renders nothing when the transcript carries no untrusted_first_call", async () => {
    project({ cwd: "/r", session: "sess-1" });
    mount(null);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByText(NOTICE)).toBeNull();
  });

  it("has no buttons: it never blocks", async () => {
    project({ cwd: "/r", session: "sess-1" });
    mount({ tool: "get_email" });
    await screen.findByText(NOTICE);
    // An optional Dismiss is the only button the card may carry.
    const buttons = screen.queryAllByRole("button");
    expect(buttons.filter((b) => !/dismiss/i.test(b.textContent ?? ""))).toHaveLength(0);
  });
});
