import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { AddLinkForm } from "./LinkPicker";
import { renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

const NOTES = [
  { id: 4, text: "Rust owns the state", origin: "shell", state: "active", created_at: "x", updated_at: "x" },
  { id: 9, text: "Rust error handling\nsecond line", origin: "shell", state: "active", created_at: "x", updated_at: "x" },
];

const PROJECTS = [
  { project_id: "alpha", project_root: "C:/work/alpha" },
  { project_id: "beta", project_root: null },
];

const CONTACTS = [
  { address: "ada@x.io", contact_id: 3, display_name: "Ada" },
  { address: "ada@home.io", contact_id: 3, display_name: "Ada" },
  { address: "bob@x.io", contact_id: 5, display_name: null },
];

function serve() {
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
    if (path.startsWith("/owner-notes/search")) return Promise.resolve(NOTES);
    if (path === "/projects") return Promise.resolve(PROJECTS);
    if (path === "/contacts") return Promise.resolve(CONTACTS);
    if (path === "/owner-notes/4/links" && init?.method === "POST") return Promise.resolve({ id: 1 });
    return Promise.reject(new Error(`unexpected ${path}`));
  });
}

function posted(): unknown {
  const call = daemon.apiFetch.mock.calls.find(([path]) => path === "/owner-notes/4/links");
  return call === undefined ? undefined : JSON.parse((call[1] as RequestInit).body as string);
}

const input = () => screen.getByLabelText("Target ref") as HTMLInputElement;
/** The picker's rows only: a native <select> exposes its <option>s with the same role. */
const rowsOf = async () => within(await screen.findByRole("listbox")).getAllByRole("option");
const chooseKind = (kind: string) => fireEvent.change(screen.getByLabelText("Target kind"), { target: { value: kind } });

beforeEach(() => {
  daemon.apiFetch.mockReset();
  serve();
});

describe("AddLinkForm", () => {
  it("lists search results as you type, never the note itself, and a chosen row is posted by its id", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    fireEvent.change(input(), { target: { value: "rust" } });

    const list = await screen.findByRole("listbox");
    const rows = Array.from(list.querySelectorAll("[role=option]"));
    // Note 4 is the one the link starts from; it is not offered as its own target.
    expect(rows).toHaveLength(1);
    expect(rows[0]?.textContent).toContain("Rust error handling");
    expect(rows[0]?.textContent).toContain("#9");

    fireEvent.click(rows[0] as Element);
    expect(input().value).toBe("Rust error handling");
    expect(screen.queryByRole("listbox")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Add link" }));
    await waitFor(() => expect(posted()).toEqual({ link_type: "relates", target_kind: "note", target_ref: "9" }));
    // Done: the form is empty again.
    await waitFor(() => expect(input().value).toBe(""));
  });

  it("filters a kind without a query parameter on the client, and arrows plus Enter pick", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    chooseKind("project");
    fireEvent.focus(input());
    expect((await rowsOf()).map((row) => row.textContent)).toEqual([
      "alphaC:/work/alpha",
      "betano root on disk yet",
    ]);

    fireEvent.change(input(), { target: { value: "bet" } });
    expect(await rowsOf()).toHaveLength(1);

    fireEvent.keyDown(input(), { key: "ArrowDown" });
    expect(input().getAttribute("aria-activedescendant")).toBe((await rowsOf())[0]?.id);
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(input().value).toBe("beta");

    fireEvent.change(screen.getByLabelText("Link type"), { target: { value: "supports" } });
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));
    await waitFor(() => expect(posted()).toEqual({ link_type: "supports", target_kind: "project", target_ref: "beta" }));
  });

  it("offers one row per contact, sent by contact id", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    chooseKind("contact");
    fireEvent.focus(input());
    const rows = await rowsOf();
    expect(rows).toHaveLength(2);
    fireEvent.click(rows[1] as Element);
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));
    await waitFor(() => expect(posted()).toEqual({ link_type: "relates", target_kind: "contact", target_ref: "5" }));
  });

  it("takes a typed ref as it is when nothing is chosen", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    chooseKind("project");
    fireEvent.change(input(), { target: { value: " 7 " } });
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));
    await waitFor(() => expect(posted()).toEqual({ link_type: "relates", target_kind: "project", target_ref: "7" }));
  });

  it("keeps a plain field for a kind with no searchable source", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    chooseKind("file");
    expect(screen.queryByRole("combobox", { name: "Target ref" })).toBeNull();
    expect(input().placeholder).toBe("id or path");

    const submit = screen.getByRole("button", { name: "Add link" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);
    fireEvent.change(input(), { target: { value: "docs/plan.md" } });
    expect(submit.disabled).toBe(false);
    fireEvent.click(submit);
    await waitFor(() => expect(posted()).toEqual({ link_type: "relates", target_kind: "file", target_ref: "docs/plan.md" }));
  });

  it("switching the kind drops the ref and fetches only the active kind", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    fireEvent.change(input(), { target: { value: "rust" } });
    await screen.findByRole("listbox");
    expect(daemon.apiFetch.mock.calls.some(([path]) => path === "/projects")).toBe(false);

    chooseKind("project");
    expect(input().value).toBe("");
    expect((screen.getByRole("button", { name: "Add link" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.focus(input());
    await rowsOf();
    expect(daemon.apiFetch.mock.calls.some(([path]) => path === "/contacts")).toBe(false);
  });

  it("Escape closes the list and keeps what was typed", async () => {
    renderWithQuery(<AddLinkForm id={4} />);
    chooseKind("project");
    fireEvent.change(input(), { target: { value: "al" } });
    await screen.findByRole("listbox");
    fireEvent.keyDown(input(), { key: "Escape" });
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(input().value).toBe("al");
  });
});
