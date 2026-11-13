import { describe, expect, it, vi, beforeEach } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const apiFetch = vi.fn();
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  apiFetch: (...args: unknown[]) => apiFetch(...args),
}));

import { OwnerDecisionsInbox } from "./OwnerDecisionsInbox";

const open = [
  { id: "d1", tree_id: "tree-a", node_id: "node-a", question: "Line 1\nLine 2", at: "2026-11-04T10:00:00+00:00", status: "open", answer: null, kind: "decision" },
  { id: "pending-1", tree_id: "tree-a", node_id: "node-b", question: "Gate log", at: "2026-11-04T10:00:00+00:00", status: "open", answer: null, kind: "notice" },
];

function renderInbox() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <OwnerDecisionsInbox />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  apiFetch.mockReset();
  let rows = open;
  apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (init?.method === "POST") {
      rows = rows.filter((r) => `/decisions/${r.id}` !== path);
      return undefined;
    }
    return rows;
  });
});

describe("OwnerDecisionsInbox", () => {
  it("badges the answerable count, leaving notices out", async () => {
    renderInbox();
    const badge = await screen.findByRole("button", { name: /decisions/i });
    await waitFor(() => expect(badge.textContent).toContain("1"));
  });

  it("shows a notice greyed with no answer box, and a decision with one", async () => {
    renderInbox();
    fireEvent.click(await screen.findByRole("button", { name: /decisions/i }));
    const notice = await screen.findByTestId("owner-decision-pending-1");
    expect(within(notice).queryByText("Gate log")).not.toBeNull();
    expect(within(notice).queryByRole("textbox")).toBeNull();
    expect(within(notice).queryByRole("button")).toBeNull();
    const decision = screen.getByTestId("owner-decision-d1");
    expect(within(decision).queryByRole("textbox")).not.toBeNull();
    expect(within(decision).queryByText(/tree-a/)).not.toBeNull();
  });

  it("sends the answer and the row disappears", async () => {
    renderInbox();
    fireEvent.click(await screen.findByRole("button", { name: /decisions/i }));
    const decision = await screen.findByTestId("owner-decision-d1");
    fireEvent.change(within(decision).getByRole("textbox"), { target: { value: "go ahead" } });
    fireEvent.click(within(decision).getByRole("button", { name: /send/i }));
    await waitFor(() =>
      expect(apiFetch).toHaveBeenCalledWith("/decisions/d1", {
        method: "POST",
        body: JSON.stringify({ answer: "go ahead" }),
      }),
    );
    await waitFor(() => expect(screen.queryByTestId("owner-decision-d1")).toBeNull());
  });
});

const FENCE_Q =
  "Branch b1 changed files outside its declared paths [core/src/**]: shell/src/a.tsx, shell/src/b.ts. Move that change to the branch that owns those paths, or have the parent widen this branch's paths.";

describe("OwnerDecisionsInbox - fence park", () => {
  function withFence() {
    const rows = [
      { ...open[0], id: "f1", question: FENCE_Q },
      open[0],
    ];
    apiFetch.mockImplementation(async (_path: string, init?: RequestInit) =>
      init?.method === "POST" ? undefined : rows,
    );
  }

  it("shows Widen paths only on a fence-park decision", async () => {
    withFence();
    renderInbox();
    fireEvent.click(await screen.findByRole("button", { name: /decisions/i }));
    const fence = await screen.findByTestId("owner-decision-f1");
    expect(within(fence).queryByRole("button", { name: "Widen paths" })).not.toBeNull();
    const plain = screen.getByTestId("owner-decision-d1");
    expect(within(plain).queryByRole("button", { name: "Widen paths" })).toBeNull();
  });

  it("sends exactly the listed files", async () => {
    withFence();
    renderInbox();
    fireEvent.click(await screen.findByRole("button", { name: /decisions/i }));
    const fence = await screen.findByTestId("owner-decision-f1");
    fireEvent.click(within(fence).getByRole("button", { name: "Widen paths" }));
    await waitFor(() =>
      expect(apiFetch).toHaveBeenCalledWith("/decisions/f1", {
        method: "POST",
        body: JSON.stringify({ answer: "widen", widen_paths: ["shell/src/a.tsx", "shell/src/b.ts"] }),
      }),
    );
  });

  it("still sends a free-text answer without widen_paths", async () => {
    withFence();
    renderInbox();
    fireEvent.click(await screen.findByRole("button", { name: /decisions/i }));
    const fence = await screen.findByTestId("owner-decision-f1");
    fireEvent.change(within(fence).getByRole("textbox"), { target: { value: "moved it" } });
    fireEvent.click(within(fence).getByRole("button", { name: /send/i }));
    await waitFor(() =>
      expect(apiFetch).toHaveBeenCalledWith("/decisions/f1", {
        method: "POST",
        body: JSON.stringify({ answer: "moved it" }),
      }),
    );
  });
});
