import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { daemonFetch, daemonState, daemonText, project, renderApp, type DaemonState } from "../test/harness";
import { settingsDouble, type SeedDoc } from "../test/settings-double";
import type { SchemaNode } from "../settings/schema";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/** The real document's shape, the `Pins` of `core/src/workflows.rs`: a `workflows` list of pins. */
const PINS_SCHEMA: SchemaNode = {
  type: "object",
  properties: {
    workflows: {
      type: "array",
      items: {
        type: "object",
        properties: {
          name: { type: "string" },
          version: { type: "string" },
          origin: { type: "string" },
          hash: { type: "string" },
        },
      },
    },
  },
};

const NEXT_PINS = {
  workflows: [{ name: "harness", version: "2.0", origin: "library", hash: "abc123" }],
};

function pinsWorld(doc: Partial<SeedDoc> = {}, overrides: Partial<DaemonState> = {}): DaemonState {
  return daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow" })],
    settings: settingsDouble([
      {
        scope: "project:nucleos",
        name: "workflow_pins",
        schema: PINS_SCHEMA,
        body: { workflows: [] },
        version: 2,
        takes_effect: "straight away",
        ...doc,
      },
    ]),
    ...overrides,
  });
}

async function openWorkflows(state: DaemonState) {
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  await renderApp({ initialPath: "/projects/nucleos/workflows" });
}

/** Through Raw JSON: this file is about where the document goes, and `FieldNode` has its own tests. */
async function typePins(body: unknown) {
  const panel = within(await screen.findByRole("region", { name: "Workflow pins" }));
  fireEvent.click(await panel.findByRole("button", { name: "Raw JSON" }));
  fireEvent.change(panel.getByLabelText("Raw JSON"), { target: { value: JSON.stringify(body) } });
  fireEvent.click(panel.getByRole("button", { name: "Save" }));
}

function workflowReads(): number {
  return daemon.apiFetch.mock.calls.filter(([path]) => path === "/projects/nucleos/workflows").length;
}

describe("the pins document on the Workflows screen", () => {
  it("offers the project's pins document, and says when it starts mattering", async () => {
    await openWorkflows(pinsWorld());

    const panel = within(await screen.findByRole("region", { name: "Workflow pins" }));
    expect(await panel.findByText("Takes effect straight away.")).toBeTruthy();
    expect(panel.getByRole("button", { name: "Save" })).toBeTruthy();
  });

  it("writes the pins to the project's document with the version it read", async () => {
    const state = pinsWorld();
    await openWorkflows(state);

    await typePins(NEXT_PINS);

    await waitFor(() => expect(state.settings.writes.length).toBe(1));
    expect(state.settings.writes[0]).toEqual({
      scope: "project:nucleos",
      name: "workflow_pins",
      body: NEXT_PINS,
      version: 2,
    });
  });

  /** The list above the form is drawn from the pins, so a save that left it alone would lie. */
  it("reads the project's workflows again once the pins are saved", async () => {
    const state = pinsWorld();
    await openWorkflows(state);
    const panel = within(await screen.findByRole("region", { name: "Workflow pins" }));
    await panel.findByText("Takes effect straight away.");
    await waitFor(() => expect(workflowReads()).toBeGreaterThan(0));
    const before = workflowReads();

    await typePins(NEXT_PINS);

    await waitFor(() => expect(workflowReads()).toBeGreaterThan(before));
  });

  it("does not read the workflows again for a refused save", async () => {
    const state = pinsWorld();
    await openWorkflows(state);
    const panel = within(await screen.findByRole("region", { name: "Workflow pins" }));
    await panel.findByText("Takes effect straight away.");
    await waitFor(() => expect(workflowReads()).toBeGreaterThan(0));
    const before = workflowReads();

    state.settings.failNext = { status: 423, body: { refusal: "kill_switch" } };
    await typePins(NEXT_PINS);

    expect(await screen.findByText(/emergency stop is engaged/)).toBeTruthy();
    expect(workflowReads()).toBe(before);
    expect(state.settings.writes).toEqual([]);
  });

  /**
   * Pins that do not parse are something somebody has to fix, so the answer that says so must be
   * able to fix them: the page used to send them to a raw editor in another mode.
   */
  it("offers the document beside the parser's words when the pins do not read, and recovers", async () => {
    const state = pinsWorld({}, { pinsError: "pins: mapping values are not allowed at line 3" });
    await openWorkflows(state);

    expect(await screen.findByText(/mapping values are not allowed at line 3/)).toBeTruthy();
    expect(screen.getByText(/pins document does not read/)).toBeTruthy();

    state.pinsError = null;
    await typePins(NEXT_PINS);

    await waitFor(() => expect(state.settings.writes.length).toBe(1));
    await waitFor(() => expect(screen.queryByText(/pins document does not read/)).toBeNull());
  });

  it("says so when this project lists no pins document", async () => {
    await openWorkflows(
      daemonState({ projects: [project({ project_id: "nucleos", mode: "shadow" })] }),
    );

    const panel = within(await screen.findByRole("region", { name: "Workflow pins" }));
    expect(await panel.findByText(/has no workflow_pins document/)).toBeTruthy();
    expect(panel.queryByRole("button", { name: "Save" })).toBeNull();
  });
});
