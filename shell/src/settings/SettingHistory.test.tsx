import { fireEvent, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ApiRefusal } from "../data/client";
import type { SettingVersion } from "../data/settings";
import { renderWithQuery } from "../test/harness";
import { SettingHistory } from "./SettingHistory";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function version(number: number, overrides: Partial<SettingVersion> = {}): SettingVersion {
  return {
    version: number,
    written_at: "2026-10-11T10:00:00Z",
    written_by: "shell",
    deleted: false,
    ...overrides,
  };
}

function open(onRestore = vi.fn(), overrides: { current?: number; blockedBecause?: string | null } = {}) {
  renderWithQuery(
    <SettingHistory
      scope="machine"
      name="email"
      current={overrides.current ?? 3}
      blockedBecause={overrides.blockedBecause ?? null}
      onRestore={onRestore}
    />,
  );
  return onRestore;
}

describe("SettingHistory", () => {
  it("asks the document's history route", async () => {
    daemon.apiFetch.mockResolvedValue([version(1)]);
    open(undefined, { current: 1 });

    await screen.findByText("version 1");
    expect(daemon.apiFetch).toHaveBeenCalledWith("/settings/machine/email/history");
  });

  it("lists newest first whatever order the answer came in, and says who wrote each", async () => {
    daemon.apiFetch.mockResolvedValue([
      version(1, { written_by: "migration" }),
      version(3),
      version(2, { written_by: "rollback" }),
    ]);
    open();

    const list = await screen.findByRole("list", { name: "Versions of email" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows.map((row) => within(row).getByText(/^version /).textContent)).toEqual([
      "version 3",
      "version 2",
      "version 1",
    ]);
    expect(within(rows[2]).getByText("migration")).toBeTruthy();
  });

  it("marks the current version and offers no way to restore it", async () => {
    daemon.apiFetch.mockResolvedValue([version(3), version(2)]);
    open();

    const list = await screen.findByRole("list", { name: "Versions of email" });
    const [current, older] = within(list).getAllByRole("listitem");
    expect(within(current).getByText("current")).toBeTruthy();
    expect(within(current).queryByRole("button")).toBeNull();
    expect(within(older).getByRole("button", { name: "Restore version 2" })).toBeTruthy();
  });

  it("hands the chosen version to the form, which owns the write", async () => {
    daemon.apiFetch.mockResolvedValue([version(3), version(2)]);
    const onRestore = open();

    fireEvent.click(await screen.findByRole("button", { name: "Restore version 2" }));

    expect(onRestore).toHaveBeenCalledWith(2);
    expect(daemon.apiFetch).toHaveBeenCalledTimes(1);
  });

  it("offers no Restore for a deleted version, which has no body to bring back", async () => {
    daemon.apiFetch.mockResolvedValue([version(3), version(2, { deleted: true })]);
    open();

    const list = await screen.findByRole("list", { name: "Versions of email" });
    const [, deleted] = within(list).getAllByRole("listitem");
    expect(within(deleted).getByText("deleted")).toBeTruthy();
    expect(within(deleted).queryByRole("button")).toBeNull();
  });

  it("disables Restore while something blocks it, and says why", async () => {
    daemon.apiFetch.mockResolvedValue([version(3), version(2)]);
    const onRestore = open(undefined, { blockedBecause: "Save or revert your edit first" });

    const button = (await screen.findByRole("button", { name: "Restore version 2" })) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toBe("Save or revert your edit first");
    fireEvent.click(button);
    expect(onRestore).not.toHaveBeenCalled();
  });

  it("says a document with no versions has never been written", async () => {
    daemon.apiFetch.mockResolvedValue([]);
    open(undefined, { current: 0 });

    expect(await screen.findByText("No versions yet: this document was never written.")).toBeTruthy();
    expect(screen.queryByRole("list")).toBeNull();
  });

  /** The handler's 404 names itself and carries no `detail`, so `client.ts` repeats the name as the detail. */
  it("says when the daemon refused the list", async () => {
    daemon.apiFetch.mockRejectedValue(
      new ApiRefusal(404, "unknown_document", "unknown_document", { refusal: "unknown_document" }),
    );
    open();

    expect(await screen.findByText(/refused this: unknown_document/)).toBeTruthy();
  });

  it("says when the daemon did not answer", async () => {
    daemon.apiFetch.mockRejectedValue(new TypeError("fetch failed"));
    open();

    expect(await screen.findByText(/is not responding/)).toBeTruthy();
  });
});
