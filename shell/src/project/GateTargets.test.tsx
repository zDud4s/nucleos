import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { settingsDouble } from "../test/settings-double";
import { GateTargets } from "./GateTargets";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

const SCHEMA = { type: "object" } as const;
const TARGETS = ["core", "sidecars", "shell", "tauri", "hooks", "broker"];

function world(body: unknown = { gate_command: "scripts/gates.sh" }, version = 5, listed = true) {
  const double = settingsDouble(
    listed
      ? [{ scope: "project:alpha", name: "autopilot", schema: SCHEMA, body, version }]
      : [{ scope: "project:alpha", name: "automations", schema: SCHEMA, body: {}, version: 1 }],
  );
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => double.handle(path, init));
  return double;
}

function open() {
  return renderWithQuery(<GateTargets projectId="alpha" />);
}

const group = (target: string) => screen.getByRole("group", { name: target });
const pick = (target: string, option: "Local" | "CI only") =>
  fireEvent.click(within(group(target)).getByRole("button", { name: option }));
const unset = (target: string) => fireEvent.click(within(group(target)).getByRole("button", { name: "Unset" }));
const pressed =(target: string, option: string) =>
  within(group(target)).getByRole("button", { name: option }).getAttribute("aria-pressed");
const save = () => fireEvent.click(screen.getByRole("button", { name: "Save targets" }));

describe("GateTargets", () => {
  it("says so when the project has no autopilot document", async () => {
    world({}, 1, false);
    open();
    expect(await screen.findByTitle("This project has no autopilot document to edit.")).toBeTruthy();
  });

  it("draws one row per known target in order, each with Local and CI only, and a read-only security row", async () => {
    world();
    open();
    await screen.findByRole("group", { name: "core" });
    // The collapsed "Details" disclosure is an unnamed group of its own; only named rows count.
    const groups = screen
      .getAllByRole("group")
      .map((g) => g.getAttribute("aria-label"))
      .filter((name) => name !== null);
    expect(groups).toEqual(TARGETS);
    for (const target of TARGETS) {
      expect(within(group(target)).getByRole("button", { name: "Local" })).toBeTruthy();
      expect(within(group(target)).getByRole("button", { name: "CI only" })).toBeTruthy();
    }
    expect(screen.getByText("CI only (cannot run locally)")).toBeTruthy();
    expect(screen.getByText("security")).toBeTruthy();
    expect(screen.queryByRole("group", { name: "security" })).toBeNull();
  });

  it("shows Not set for every target when the document has no gate_targets", async () => {
    world();
    open();
    await screen.findByRole("group", { name: "core" });
    expect(screen.getAllByText("Not set").length).toBe(TARGETS.length);
    expect(pressed("core", "Local")).toBe("false");
    expect(pressed("core", "CI only")).toBe("false");
    expect((screen.getByRole("button", { name: "Save targets" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("choosing places and saving PUTs the whole autopilot document with gate_targets at the version read", async () => {
    const double = world({ gate_command: "scripts/gates.sh", release_branch: "main" }, 5);
    open();
    await screen.findByRole("group", { name: "core" });
    pick("core", "Local");
    pick("shell", "CI only");
    save();

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0]).toEqual({
      scope: "project:alpha",
      name: "autopilot",
      version: 5,
      body: { gate_command: "scripts/gates.sh", release_branch: "main", gate_targets: { core: "local", shell: "ci" } },
    });
  });

  it("shows an existing gate_targets mapping preselected", async () => {
    world({ gate_targets: { core: "local", tauri: "ci" } });
    open();
    await screen.findByRole("group", { name: "core" });
    expect(pressed("core", "Local")).toBe("true");
    expect(pressed("core", "CI only")).toBe("false");
    expect(pressed("tauri", "CI only")).toBe("true");
    // Once a mapping is stored, a target left out of it runs in CI rather than being undecided.
    expect(screen.getAllByText("Not set (runs in CI)").length).toBe(TARGETS.length - 2);
  });

  it("clearing the only stored target and saving omits gate_targets from the body", async () => {
    const double = world({ gate_command: "scripts/gates.sh", gate_targets: { core: "local" } }, 7);
    open();
    await screen.findByRole("group", { name: "core" });
    unset("core");
    expect(screen.getAllByText("Not set").length).toBe(TARGETS.length);
    save();

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].version).toBe(7);
    expect(double.writes[0].body).toEqual({ gate_command: "scripts/gates.sh" });
    expect("gate_targets" in (double.writes[0].body as object)).toBe(false);
  });

  it("clearing one of two stored targets keeps the other", async () => {
    const double = world({ gate_targets: { core: "local", tauri: "ci" } });
    open();
    await screen.findByRole("group", { name: "core" });
    unset("tauri");
    save();

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].body).toEqual({ gate_targets: { core: "local" } });
  });

  it("shows the núcleo's 422 sentence next to the table and keeps the choices", async () => {
    const double = world();
    open();
    await screen.findByRole("group", { name: "core" });
    pick("core", "CI only");
    double.failNext = {
      status: 422,
      body: { refusal: "invalid", detail: "at least one target must run locally" },
    };
    save();

    expect(await screen.findByText("at least one target must run locally")).toBeTruthy();
    expect(screen.getByRole("alert")).toBeTruthy();
    expect(pressed("core", "CI only")).toBe("true");
  });

  it("a stale version reloads and says so", async () => {
    const double = world();
    open();
    await screen.findByRole("group", { name: "core" });
    pick("core", "Local");
    double.external("project:alpha", "autopilot", { gate_command: "x", gate_targets: { shell: "local" } });
    save();

    expect(await screen.findByText(/changed the autopilot rules/)).toBeTruthy();
    await waitFor(() => expect(pressed("shell", "Local")).toBe("true"));
  });

  it("never renders a textarea, a JSON box or a free-text key field", async () => {
    world();
    const { container } = open();
    await screen.findByRole("group", { name: "core" });
    expect(container.querySelector("textarea")).toBeNull();
    expect(container.querySelector("input")).toBeNull();
    expect(screen.queryByText(/Raw JSON/)).toBeNull();
  });
});
