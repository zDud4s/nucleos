import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { renderWithRouter } from "../test/harness";
import { GlobalSearch } from "./GlobalSearch";

const PALETTE = { name: "Go to anything" };

function answer(path: string): Promise<unknown> {
  if (path.startsWith("/assistant/search")) {
    return Promise.resolve([
      {
        chat_id: "chat-9",
        title: "Planning",
        turn_id: 4,
        side: "answered",
        excerpt: "the merge-gate is closed",
        created_at: "2026-10-19T10:00:00Z",
      },
    ]);
  }
  if (path.startsWith("/runs")) {
    return Promise.resolve([
      { id: 7, status: "done", prompt_excerpt: "Investigate the merge-gate" },
    ]);
  }
  if (path.startsWith("/email/queue")) {
    return Promise.resolve([
      { id: 12, from_addr: "conta@firma.pt", from_name: "Contabilista", subject: "IVA do merge" },
    ]);
  }
  if (path.startsWith("/owner-notes/search")) {
    return Promise.resolve([{ id: 3, text: "Remember the merge-gate rule" }]);
  }
  return Promise.resolve([]);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(answer);
});

async function openAndType(value: string): Promise<HTMLElement> {
  fireEvent.keyDown(window, { key: "k", ctrlKey: true });
  const palette = await screen.findByRole("dialog", PALETTE);
  fireEvent.change(within(palette).getByRole("combobox"), { target: { value } });
  return palette;
}

describe("GlobalSearch", () => {
  it("typing in the palette lists runs, mail and notes that match", async () => {
    await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    const palette = await openAndType("merge-gate");

    expect(await within(palette).findByText("Investigate the merge-gate")).toBeDefined();
    expect(within(palette).getByText("IVA do merge")).toBeDefined();
    expect(within(palette).getByText("Remember the merge-gate rule")).toBeDefined();
    expect(within(palette).getByText("Runs")).toBeDefined();
    expect(within(palette).getByText("Mail")).toBeDefined();
    expect(within(palette).getByText("Notes")).toBeDefined();
    // A source with nothing to say draws no heading.
    expect(within(palette).queryByText("Saved web pages")).toBeNull();
  });

  it("choosing a run hit opens its page", async () => {
    const { router } = await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    const palette = await openAndType("merge-gate");

    fireEvent.click(await within(palette).findByText("Investigate the merge-gate"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/runs/7"));
    await waitFor(() => expect(screen.queryByRole("dialog", PALETTE)).toBeNull());
  });

  it("choosing a feed or note hit goes where the plan says", async () => {
    daemon.apiFetch.mockImplementation((path: string) =>
      path.startsWith("/feed")
        ? Promise.resolve([{ id: 5, kind: "run_done", summary: "merge-gate passed" }])
        : answer(path),
    );
    const { router } = await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    const palette = await openAndType("merge-gate");

    fireEvent.click(await within(palette).findByText("merge-gate passed"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/feed"));
    expect(router.state.location.search).toMatchObject({ q: "merge-gate" });
  });

  it("on /chats the conversation hits are not listed twice", async () => {
    await renderWithRouter(<GlobalSearch />, { initialPath: "/chats" });
    const palette = await openAndType("merge-gate");

    await within(palette).findByText("Investigate the merge-gate");
    expect(within(palette).queryByText("Said in a conversation")).toBeNull();
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    expect(asked.some((path) => path.startsWith("/assistant/search"))).toBe(false);
  });

  it("off /chats the conversation hits are listed and open their chat", async () => {
    const { router } = await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    const palette = await openAndType("merge-gate");

    expect(await within(palette).findByText("Said in a conversation")).toBeDefined();
    fireEvent.click(within(palette).getByText("the merge-gate is closed"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats/chat-9"));
  });

  it("waits for the typing to pause before asking", async () => {
    await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    const palette = await openAndType("mer");
    fireEvent.change(within(palette).getByRole("combobox"), { target: { value: "merge" } });
    fireEvent.change(within(palette).getByRole("combobox"), { target: { value: "merge-gate" } });

    // Nothing has been asked while the keys are still going down.
    expect(daemon.apiFetch).not.toHaveBeenCalled();

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    expect(asked.every((path) => path.includes("merge-gate"))).toBe(true);
  });

  it("two characters ask nothing", async () => {
    await renderWithRouter(<GlobalSearch />, { initialPath: "/" });
    await openAndType("me");

    await new Promise((resolve) => setTimeout(resolve, 350));
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});
