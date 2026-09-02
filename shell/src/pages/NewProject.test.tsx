// §spec workspace-de-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  bundle,
  daemonFetch,
  daemonState,
  daemonText,
  detected,
  renderApp,
  type DaemonState,
} from "../test/harness";
import { suggestedId } from "../data/detect";

/** Open the wizard over a daemon holding these facts, and point it at a folder. */
async function openWizard(overrides: Partial<DaemonState> = {}) {
  const state = daemonState(overrides);
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath: "/projects/new" });
  return { state, rendered };
}

async function look(path = "C:/Projects/thing") {
  fireEvent.change(await screen.findByLabelText("Folder"), { target: { value: path } });
  fireEvent.click(screen.getByRole("button", { name: "look" }));
}

describe("suggestedId", () => {
  /**
   * A suggestion and not a rule — the field stays editable. Both separators, because this daemon
   * runs on Windows and a path arrives with either.
   */
  it("proposes the folder's own name, in a shape a project id can have", () => {
    expect(suggestedId("C:/Projects/NucleOS")).toBe("nucleos");
    expect(suggestedId("C:\\Projects\\My Thing\\")).toBe("my-thing");
    expect(suggestedId("/home/x/some_repo")).toBe("some-repo");
  });
});

describe("adding a project", () => {
  /**
   * **§9's second step, and the reason it exists.** This repository's way of working is a folder,
   * and the app has to recognise it rather than ask for it to be described again.
   *
   * Adopting copies nothing, so the sentence has to say what it does give up — updates — because a
   * pin that quietly never updates is the silence §6.1 spends a section on.
   */
  it("recognises the way of working a project already has, and says what adopting costs", async () => {
    await openWizard({
      detected: detected({
        is_git: true,
        branch: "master",
        harnesses: [
          { path: ".ai", what: "a written-down pipeline", files: 42 },
          { path: ".claude", what: "skills and commands", files: 7 },
        ],
      }),
    });
    await look();

    expect(await screen.findByText(".ai")).toBeTruthy();
    expect(screen.getByText("42 files")).toBeTruthy();
    expect(screen.getByText(/receives no updates/)).toBeTruthy();
    expect(screen.getByText(/not copied, not rewritten/)).toBeTruthy();
    // The first one found is preselected, and declining all of them is an option rather than an
    // omission — a project may keep its folder and use no workflow.
    expect(screen.getByRole("radio", { name: /adopt none of them/ })).toBeTruthy();
  });

  /**
   * The whole flow, asserted on what was SENT. Four routes in one order, and the order is the
   * design: the project row first, because everything after it is addressed by the id it creates.
   *
   * **Shadow, always.** §9 says so, and the reason is the design's restraint: a project that started
   * acting on its own the moment it was added would be one nobody had decided to trust yet.
   */
  it("registers in shadow, adopts the folder, declares what was ticked, and sets the ceiling", async () => {
    const { state, rendered } = await openWizard({
      detected: detected({
        root: "C:/Projects/nucleos",
        is_git: true,
        branch: "master",
        harnesses: [{ path: ".ai", what: "a written-down pipeline", files: 42 }],
        commands: [
          { name: "gate", command: "cargo test", source: "Makefile" },
          { name: "fmt", command: "cargo fmt", source: "Makefile" },
        ],
      }),
    });
    await look("C:/Projects/nucleos");

    fireEvent.click(await screen.findByRole("checkbox", { name: /gate/ }));
    fireEvent.change(screen.getByLabelText("WIP ceiling"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "add it, in shadow" }));

    await waitFor(() => expect(state.projects.length).toBeGreaterThan(0));
    // The folder the filesystem resolved, not the string that was typed.
    expect(state.projects[0]).toMatchObject({
      project_id: "nucleos",
      mode: "shadow",
      project_root: "C:/Projects/nucleos",
    });
    await waitFor(() => expect(state.adopted).toEqual([{ name: "harness", path: ".ai" }]));

    // Only what was ticked, and nothing arrives as a gate: saying a command's result decides
    // whether the project is green is a claim to make deliberately.
    await waitFor(() => expect(state.declared.length).toBe(1));
    expect(state.declared[0]).toMatchObject({ name: "gate", is_gate: false });
    await waitFor(() => expect(state.projects[0].wip_limit).toBe(3));

    // And it lands on the project it just made.
    await waitFor(() =>
      expect(rendered.router.state.location.pathname).toBe("/projects/nucleos/state"),
    );
  });

  /**
   * One folder under two names is the mistake nothing else can catch: neither name looks wrong on
   * its own, and the folder cannot say it has been claimed.
   */
  it("refuses to add a folder this daemon already watches, and says which project has it", async () => {
    await openWizard({ detected: detected({ taken_by: "nucleos" }) });
    await look();

    // Twice on the page and deliberately: once in the banner explaining what would go wrong, and
    // once beside the button that is disabled because of it.
    expect((await screen.findAllByText(/already registered as/)).length).toBe(2);
    expect(screen.getByRole("button", { name: "add it, in shadow" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  /**
   * A folder with a workflow already in it is not offered one from the library: it has one, and
   * offering both would be asking somebody to choose between a thing they have and a thing they
   * have not read.
   */
  it("offers the library only when the folder has no way of working of its own", async () => {
    const first = await openWizard({ detected: detected(), library: [bundle()] });
    await look();
    expect(await screen.findByLabelText("Workflow")).toBeTruthy();
    // Unmounted before the second mount: `cleanup` runs between tests, not inside one, and two
    // apps in one document would make every query ambiguous.
    first.rendered.unmount();

    await openWizard({
      detected: detected({ harnesses: [{ path: ".ai", what: "a pipeline", files: 3 }] }),
      library: [bundle()],
    });
    await look();
    await screen.findByText(".ai");
    expect(screen.queryByLabelText("Workflow")).toBeNull();
  });

  /**
   * Not a repository is a finding, not a refusal — `set_project_mode` only insists on one for
   * `active` — and the page says which half of the future that closes off.
   */
  it("treats a folder that is not a repository as a finding rather than a refusal", async () => {
    await openWizard({ detected: detected({ is_git: false }) });
    await look();
    expect(await screen.findByText(/only ever run in shadow/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "add it, in shadow" })).toHaveProperty(
      "disabled",
      false,
    );
  });

  /** Three refusals, three sentences: they send somebody to three different places. */
  it("says which of the three reasons a folder could not be read", async () => {
    await openWizard({ detected: null });
    await look("C:/nowhere");
    expect(await screen.findByText(/nothing at that path/)).toBeTruthy();
  });

  /**
   * A failure part-way through stops and says so. Carrying on would leave a project registered,
   * half-configured, with nothing on screen saying which half.
   */
  it("stops at the first refusal instead of leaving a project half-configured", async () => {
    const { state } = await openWizard({
      detected: detected({
        harnesses: [{ path: ".ai", what: "a pipeline", files: 3 }],
        commands: [{ name: "gate", command: "cargo test", source: "Makefile" }],
      }),
      workflowRefusal: { status: 423, code: "kill_switch", detail: "the stop is engaged" },
    });
    await look();

    fireEvent.click(await screen.findByRole("checkbox", { name: /gate/ }));
    fireEvent.click(screen.getByRole("button", { name: "add it, in shadow" }));

    expect(await screen.findByText(/the stop is engaged/)).toBeTruthy();
    // The project row was written — that step succeeded — and nothing after the refusal ran.
    expect(state.projects.length).toBe(1);
    expect(state.declared).toEqual([]);
  });
});
