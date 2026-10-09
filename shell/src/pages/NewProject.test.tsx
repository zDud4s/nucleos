// §spec workspace-de-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

const tauri = vi.hoisted(() => ({ isTauri: vi.fn(() => false), pickFolder: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: tauri.isTauri }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: tauri.pickFolder }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import {
  bundle,
  daemonFetch,
  daemonState,
  daemonText,
  detected,
  project,
  renderApp,
  type DaemonState,
} from "../test/harness";
import { suggestedId } from "../data/detect";

/** Open the wizard over a daemon holding these facts, and point it at a folder. */
async function openWizard(overrides: Partial<DaemonState> = {}, initialPath = "/projects/new") {
  const state = daemonState(overrides);
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath });
  return { state, rendered };
}

async function look(path = "C:/Projects/thing") {
  fireEvent.change(await screen.findByLabelText("Folder"), { target: { value: path } });
  fireEvent.click(screen.getByRole("button", { name: "Look" }));
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
  it("a step says which of how many", async () => {
    await openWizard();

    expect(await screen.findByRole("heading", { name: "1 of 3. The folder" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "The folder" })).toBeTruthy();
  });

  /**
   * §9's second step, and the reason it exists. This repository's way of working is a folder,
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

    // By its radio: the path is also in the receipt of what adding will write.
    expect(await screen.findByRole("radio", { name: /\.ai/ })).toBeTruthy();
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
    // The same name and the same control as the Settings block this number is found under next.
    const ceiling = screen.getByRole("group", { name: "Open-proposal ceiling" });
    fireEvent.click(within(ceiling).getByRole("button", { name: "Raise the ceiling" }));
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));

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
    const add = screen.getByRole("button", { name: "Add it, in shadow" });
    expect(add).toHaveProperty("disabled", true);
    // The reason is the button's description, not a span a screen reader never reaches.
    const reason = document.getElementById(add.getAttribute("aria-describedby") ?? "");
    expect(reason?.textContent).toBe("already registered as nucleos");
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
    await screen.findByRole("radio", { name: /\.ai/ });
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
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty(
      "disabled",
      false,
    );
  });

  /**
   * Three refusals, three sentences: they send somebody to three different places. A refusal is
   * the daemon answering, so it is a status and not an alert.
   */
  it("says which of the three reasons a folder could not be read", async () => {
    await openWizard({ detected: null });
    await look("C:/nowhere");
    const note = (await screen.findByText(/nothing at that path/)).closest("p");
    expect(note?.getAttribute("role")).toBe("status");
    expect(within(note as HTMLElement).getByText("no_such_folder")).toBeTruthy();
  });

  /**
   * A failure part-way through stops and says so — and says what already landed. Carrying on would
   * leave a project registered, half-configured, with nothing on screen saying which half; and a
   * button left inviting a second press would repeat the registration. So the receipt marks the
   * line that stopped, the button goes, and the way on is the project's own page.
   */
  it("stops at the first refusal, says what was already written, and does not invite a retry", async () => {
    const { state } = await openWizard({
      detected: detected({
        harnesses: [{ path: ".ai", what: "a pipeline", files: 3 }],
        commands: [{ name: "gate", command: "cargo test", source: "Makefile" }],
      }),
      workflowRefusal: { status: 423, code: "kill_switch", detail: "the stop is engaged" },
    });
    await look();

    fireEvent.click(await screen.findByRole("checkbox", { name: /gate/ }));
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));

    expect(await screen.findByText(/Registered as/)).toBeTruthy();
    expect(screen.getByText(/Registered as/).textContent).toBe(
      "Registered as thing in shadow, and stopped at adopting .ai. Nothing after it was tried.",
    );
    expect(screen.getByText(/kill switch is engaged/)).toBeTruthy();
    expect(screen.getByText("kill_switch")).toBeTruthy();
    expect(screen.getAllByText("done").length).toBe(1);
    expect(screen.getByText("stopped here")).toBeTruthy();
    expect(screen.getAllByText("not tried").length).toBe(2);

    // The project row was written — that step succeeded — and nothing after the refusal ran.
    expect(state.projects.length).toBe(1);
    expect(state.declared).toEqual([]);

    // No second press on offer: it would register again and re-send what landed.
    expect(screen.queryByRole("button", { name: "Add it, in shadow" })).toBeNull();
    expect(
      screen.getByRole("link", { name: "Finish setting up thing on its page" }).getAttribute("href"),
    ).toBe("/projects/thing/state");
  });

  /** A refusal on the very first write left nothing behind, so pressing again is a retry. */
  it("keeps the button when the registration itself was refused", async () => {
    const { state } = await openWizard({ detected: detected() });
    await look();
    await screen.findByRole("button", { name: "Add it, in shadow" });
    const answers = daemonFetch(state);
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
      if (path === "/autopilot/state" && init?.method === "POST") {
        return Promise.reject(new ApiRefusal(422, "unprocessable", ""));
      }
      return answers(path, init);
    });
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));

    expect(await screen.findByText(/did not say which prerequisite is missing/)).toBeTruthy();
    expect(screen.queryByText(/Registered as/)).toBeNull();
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", false);
  });

  /**
   * The stop refuses the workflow and nothing else of the four, so it is said before the first
   * write — not found out after the project row already exists — and the way round it is offered.
   *
   * Onboarding, inside the register step, is refused too — it now consults the same switch, like
   * every other governance write — but that refusal is caught rather than fatal: the project still
   * registers, and the page says onboarding is waiting rather than sending the person to their
   * new project's page as if nothing had been skipped.
   */
  it("warns about an engaged kill switch before anything is written, and can leave the workflow out", async () => {
    const { state } = await openWizard({
      kill: { engaged: true },
      detected: detected({ harnesses: [{ path: ".ai", what: "a pipeline", files: 3 }] }),
      onboardRefusal: { status: 423, code: "kill_switch", detail: "the stop is engaged" },
    });
    await look();

    expect(await screen.findByText(/while it is the núcleo refuses to record a workflow/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", true);
    expect(screen.getByText("the kill switch would refuse the workflow")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "leave the workflow for later" }));
    expect(screen.getByRole("radio", { name: /adopt none of them/ })).toHaveProperty("checked", true);
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));

    await waitFor(() => expect(state.projects.length).toBe(1));
    expect(state.adopted).toEqual([]);
    // Onboarding was skipped, not landed — the marker was never written.
    expect(state.onboarded).toEqual([]);
    expect(
      await screen.findByText(/onboarding.*is waiting until it is released/),
    ).toBeTruthy();
    expect(
      screen.getByRole("link", { name: "Go to thing" }).getAttribute("href"),
    ).toBe("/projects/thing/state");
  });

  /**
   * The id goes into every URL the project has. Its shape is the one `suggestedId` produces, it is
   * checked here rather than by a refusal at the bottom of the page, and the nearest good one is
   * offered. The visible label is the accessible name, so "click project id" finds it.
   */
  it("refuses an id out of shape before anything is sent, and offers the nearest one", async () => {
    const { state } = await openWizard({ detected: detected() });
    await look();

    const id = (await screen.findByLabelText("Project id")) as HTMLInputElement;
    fireEvent.change(id, { target: { value: "My Project" } });
    expect(id.getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", true);
    expect(screen.getByText(/not an id yet/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "use my-project" }));
    expect(id.value).toBe("my-project");
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));
    await waitFor(() => expect(state.projects[0]?.project_id).toBe("my-project"));
  });

  /**
   * Registering is an upsert, so an id another project has would not be refused — it would re-point
   * that project at this folder. The one mistake here nothing downstream catches.
   */
  it("will not reuse the id of a project that already exists", async () => {
    await openWizard({ projects: [project({ project_id: "thing" })], detected: detected() });
    await look();

    expect(await screen.findByText(/would move that project here/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", true);
  });

  /** "No ceiling" is a real answer on the project's page, so it is one here too — sent as null. */
  it("can add a project with no open-proposal ceiling", async () => {
    const { state } = await openWizard({ detected: detected() });
    await look();

    fireEvent.click(await screen.findByRole("button", { name: "no ceiling" }));
    expect(screen.getByText("off")).toBeTruthy();
    expect(screen.getByText("no open-proposal ceiling")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));
    await waitFor(() => expect(state.projects[0]).toMatchObject({ wip_limit: null }));
  });

  /** The receipt, read before signing: the writes, in the order they will be made. */
  it("lists what adding will write before the button is pressed", async () => {
    await openWizard({
      detected: detected({
        harnesses: [{ path: ".ai", what: "a pipeline", files: 3 }],
        commands: [{ name: "gate", command: "cargo test", source: "Makefile" }],
      }),
    });
    await look();
    fireEvent.click(await screen.findByRole("checkbox", { name: /gate/ }));

    const receipt = screen.getByText("Adding it will:").nextElementSibling as HTMLElement;
    expect(within(receipt).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
      "register thing at C:/Projects/thing, in shadow, onboarded with no gate command",
      "adopt .ai as its way of working",
      "declare gate, not as a gate",
      "an open-proposal ceiling of 2",
    ]);
  });

  /**
   * Onboarding is part of registering: the mode door refuses a project nobody onboarded. The gate
   * starts from the núcleo's proposal, is sent as the person left it, and blank is none.
   */
  it("onboards with the gate as confirmed before it registers", async () => {
    const { state } = await openWizard({
      detected: detected({ gate: { command: "npm run test", source: "package.json" } }),
    });
    await look();

    const gate = (await screen.findByLabelText("Gate command")) as HTMLInputElement;
    expect(gate.value).toBe("npm run test");
    expect(screen.getByText(/proposed from package\.json/)).toBeTruthy();
    fireEvent.change(gate, { target: { value: "npm run ci" } });
    expect(screen.getByText(/onboarded with the gate/).textContent).toContain("npm run ci");

    fireEvent.click(screen.getByRole("button", { name: "Add it, in shadow" }));
    await waitFor(() => expect(state.projects.length).toBe(1));
    expect(state.onboarded).toEqual([
      { projectId: "thing", project_root: "C:/Projects/thing", gate_command: "npm run ci" },
    ]);
  });

  /** A refused onboarding registers nothing, and pressing again is a retry. */
  it("registers nothing when onboarding is refused", async () => {
    await openWizard({
      detected: detected(),
      onboardRefusal: { status: 409, code: "hook_unwritable", detail: "settings.json is not JSON" },
    });
    await look();
    fireEvent.click(await screen.findByRole("button", { name: "Add it, in shadow" }));

    expect(await screen.findByText("hook_unwritable")).toBeTruthy();
    expect(screen.queryByText(/Registered as/)).toBeNull();
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", false);
  });

  /** Steps two and three arrive below the focus; focus goes to them, so they are heard. */
  it("moves focus to what was found", async () => {
    await openWizard({ detected: detected() });
    await look();
    const heading = await screen.findByRole("heading", { name: "2 of 3. What is already there" });
    await waitFor(() => expect(document.activeElement).toBe(heading));
  });

  /** Edited after it was read, the folder above is not the one steps two and three describe. */
  it("holds the button when the path is edited after it was read", async () => {
    await openWizard({ detected: detected() });
    await look();
    await screen.findByRole("button", { name: "Add it, in shadow" });

    fireEvent.change(screen.getByLabelText("Folder"), { target: { value: "C:/Projects/other" } });
    expect(screen.getByRole("button", { name: "Add it, in shadow" })).toHaveProperty("disabled", true);
    expect(screen.getByText(/edited after it was read/)).toBeTruthy();
  });

  /** A page that sends somebody here with a folder in mind hands it over, and it is read at once. */
  it("takes the folder from ?path= and reads it", async () => {
    await openWizard(
      { detected: detected({ root: "C:/Projects/back" }) },
      "/projects/new?path=C%3A%2FProjects%2Fback",
    );

    expect(((await screen.findByLabelText("Folder")) as HTMLInputElement).value).toBe(
      "C:/Projects/back",
    );
    expect(await screen.findByDisplayValue("back")).toBeTruthy();
  });

  /** Inside the app, the folder can be chosen in the system's own picker, and is read at once. */
  it("browses for the folder with the native picker and reads what was chosen", async () => {
    tauri.isTauri.mockReturnValue(true);
    tauri.pickFolder.mockResolvedValue("C:/Projects/picked");
    try {
      await openWizard({ detected: detected({ root: "C:/Projects/picked" }) });
      fireEvent.click(await screen.findByRole("button", { name: "Browse…" }));

      await waitFor(() =>
        expect(((screen.getByLabelText("Folder")) as HTMLInputElement).value).toBe("C:/Projects/picked"),
      );
      expect(tauri.pickFolder).toHaveBeenCalledWith(expect.objectContaining({ directory: true, multiple: false }));
      expect(await screen.findByDisplayValue("picked")).toBeTruthy();
    } finally {
      tauri.isTauri.mockReturnValue(false);
    }
  });

  /** Outside it there is no picker that answers with a path, so the button says so instead of failing silently. */
  it("outside the app, Browse explains that the picker lives in the desktop app", async () => {
    tauri.pickFolder.mockClear();
    await openWizard();
    fireEvent.click(await screen.findByRole("button", { name: "Browse…" }));

    expect(await screen.findByText(/only opens in the desktop app/)).toBeTruthy();
    expect(tauri.pickFolder).not.toHaveBeenCalled();
  });
});
