import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { Chain } from "./Chain";
import { daemonWith, known } from "./test-helpers";
import { renderWithRouter } from "../../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("Chain", () => {
  it("says a row that replaced nothing replaced nothing", async () => {
    daemon.apiFetch.mockImplementation(daemonWith([known({ id: 4 })]));

    await renderWithRouter(<Chain id={4} />);

    expect(
      await screen.findByText("This one replaced nothing and nothing has replaced it."),
    ).toBeDefined();
  });

  it("lists what the row replaced", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 4 })], { replaced: [known({ id: 3, title: "the older text" })] }),
    );

    await renderWithRouter(<Chain id={4} />);

    expect(await screen.findByText("the older text")).toBeDefined();
  });
});
