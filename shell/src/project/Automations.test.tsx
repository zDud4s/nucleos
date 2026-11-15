import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { settingsDouble } from "../test/settings-double";
import { Automations } from "./Automations";
import type { Rule } from "./automations-model";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

// The component never reads the schema, so a minimal one is enough.
const SCHEMA = { type: "object" } as const;

const PUSH: Rule = {
  name: "push-when-green",
  enabled: false,
  on: { event: "published_green" },
  do: { action: "push", remote: "origin" },
};

function world(body: unknown = { rules: [] }, version = 3, listed = true) {
  const double = settingsDouble(
    listed
      ? [{ scope: "project:alpha", name: "automations", schema: SCHEMA, body, version }]
      : [{ scope: "project:alpha", name: "autopilot", schema: SCHEMA, body: {}, version: 1 }],
  );
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => double.handle(path, init));
  return double;
}

function open() {
  return renderWithQuery(<Automations projectId="alpha" />);
}

const box = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const choose = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
const press = (name: string) => fireEvent.click(screen.getByRole("button", { name }));

async function addForm() {
  fireEvent.click(await screen.findByRole("button", { name: "Add automation" }));
}

describe("Automations", () => {
  it("shows the empty state and an Add automation button when no rule exists", async () => {
    world(null, 0);
    const first = open();
    expect(await screen.findByText(/No automations yet/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add automation" })).toBeTruthy();
    first.unmount();

    world({ rules: [] });
    open();
    expect(await screen.findByText(/No automations yet/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add automation" })).toBeTruthy();
  });

  it("adds a push rule with the controls and PUTs the typed document at the version it read", async () => {
    const double = world();
    open();
    await addForm();

    fireEvent.change(box("Name"), { target: { value: "push-quando-verde" } });
    expect((screen.getByLabelText("When") as HTMLSelectElement).value).toBe("published_green");
    expect((screen.getByLabelText("Then") as HTMLSelectElement).value).toBe("push");
    expect(box("Remote").value).toBe("origin");
    press("Save rule");

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes).toEqual([
      {
        scope: "project:alpha",
        name: "automations",
        version: 3,
        body: {
          rules: [
            {
              name: "push-quando-verde",
              enabled: false,
              on: { event: "published_green" },
              do: { action: "push", remote: "origin" },
            },
          ],
        },
      },
    ]);
  });

  it("switching When to ci_finished hides Push and shows Open an incident, Workflow, Conclusion and the automation checkbox", async () => {
    const double = world();
    open();
    await addForm();

    expect(screen.queryByLabelText("Workflow")).toBeNull();
    expect(screen.queryByLabelText("Conclusion")).toBeNull();
    choose("When", "ci_finished");

    expect(screen.queryByRole("option", { name: "Push" })).toBeNull();
    expect(screen.getByRole("option", { name: "Open an incident" })).toBeTruthy();
    expect(box("Workflow").value).toBe("gates");
    expect(screen.getByLabelText("Conclusion")).toBeTruthy();
    expect(screen.getByLabelText("Only runs of pushes made by an automation")).toBeTruthy();

    fireEvent.change(box("Name"), { target: { value: "incident-on-red" } });
    fireEvent.click(screen.getByLabelText("Fix it automatically"));
    choose("Conclusion", "failure");
    press("Save rule");

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect((double.writes[0].body as { rules: unknown[] }).rules[0]).toEqual({
      name: "incident-on-red",
      enabled: false,
      on: { event: "ci_finished", workflow: "gates" },
      when: { conclusion: "failure" },
      do: { action: "open_incident", auto_fix: true },
    });
  });

  it("switching back to published_green hides Conclusion and the automation checkbox again and restores Push", async () => {
    world();
    open();
    await addForm();

    choose("When", "ci_finished");
    choose("Conclusion", "failure");
    choose("When", "published_green");

    expect(screen.queryByLabelText("Conclusion")).toBeNull();
    expect(screen.queryByLabelText("Only runs of pushes made by an automation")).toBeNull();
    expect(screen.queryByLabelText("Workflow")).toBeNull();
    expect(screen.getByRole("option", { name: "Push" })).toBeTruthy();
    expect((screen.getByLabelText("Then") as HTMLSelectElement).value).toBe("push");
  });

  it("lists each rule with its name, a switch and the one-line summary", async () => {
    world({ rules: [PUSH] });
    open();

    expect(await screen.findByText("push-when-green")).toBeTruthy();
    expect(screen.getByRole("switch", { name: "Enable push-when-green" })).toBeTruthy();
    expect(screen.getByText("When the published branch is green → push to origin")).toBeTruthy();
  });

  it("toggling Enable <name> PUTs the whole document with enabled flipped, at the version read", async () => {
    const double = world({ rules: [PUSH] });
    open();

    fireEvent.click(await screen.findByRole("switch", { name: "Enable push-when-green" }));

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].version).toBe(3);
    expect(double.writes[0].body).toEqual({ rules: [{ ...PUSH, enabled: true }] });
  });

  it("Edit <name> opens the form filled in and saving PUTs the changed rule in place", async () => {
    const other: Rule = { ...PUSH, name: "second", on: { event: "published_green" }, when: { branch: "dev" } };
    const double = world({ rules: [PUSH, other] });
    open();

    fireEvent.click(await screen.findByRole("button", { name: "Edit push-when-green" }));
    expect(box("Name").value).toBe("push-when-green");
    expect(box("Remote").value).toBe("origin");
    fireEvent.change(box("Remote"), { target: { value: "upstream" } });
    press("Save rule");

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].body).toEqual({
      rules: [{ ...PUSH, do: { action: "push", remote: "upstream" } }, other],
    });
  });

  it("deleting asks first: the first press arms Delete <name>?, only the confirm PUTs the list without the rule", async () => {
    const double = world({ rules: [PUSH] });
    open();

    fireEvent.click(await screen.findByRole("button", { name: "Delete push-when-green" }));
    const armed = screen.getByRole("button", { name: "Delete push-when-green? This cannot be undone." });
    expect(double.writes.length).toBe(0);

    // The interlock ignores a confirm inside its 300 ms dwell.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(armed);

    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].body).toEqual({ rules: [] });
  });

  it("shows the núcleo's 422 sentence next to the form and keeps the draft", async () => {
    const double = world();
    open();
    await addForm();

    fireEvent.change(box("Name"), { target: { value: "dup" } });
    double.failNext = {
      status: 422,
      body: { refusal: "invalid", detail: "two push rules share the branch master" },
    };
    press("Save rule");

    expect(await screen.findByText("two push rules share the branch master")).toBeTruthy();
    expect(box("Name").value).toBe("dup");
    expect(screen.getByRole("button", { name: "Save rule" })).toBeTruthy();
  });

  it("a stale version reloads the list and says so", async () => {
    const double = world();
    open();
    await addForm();

    fireEvent.change(box("Name"), { target: { value: "mine" } });
    double.external("project:alpha", "automations", { rules: [{ ...PUSH, name: "theirs" }] });
    press("Save rule");

    expect(await screen.findByText(/changed the automations/)).toBeTruthy();
    expect(await screen.findByText("theirs")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Save rule" })).toBeNull();
    expect(double.writes.length).toBe(0);
  });

  it("never renders a textarea, a JSON box or a Raw JSON button, with rules, empty, or the form open", async () => {
    world({ rules: [PUSH] });
    const populated = open();
    await screen.findByText("push-when-green");
    expect(populated.container.querySelector("textarea")).toBeNull();
    expect(screen.queryByText(/raw json/i)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Edit push-when-green" }));
    expect(populated.container.querySelector("textarea")).toBeNull();
    expect(screen.queryByText(/raw json/i)).toBeNull();
    populated.unmount();

    world({ rules: [] });
    const empty = open();
    await addForm();
    expect(empty.container.querySelector("textarea")).toBeNull();
    expect(screen.queryByText(/raw json/i)).toBeNull();
    choose("When", "ci_finished");
    expect(empty.container.querySelector("textarea")).toBeNull();
    expect(screen.queryByText(/raw json/i)).toBeNull();
  });

  it("says the project has no automations document when the index does not list it", async () => {
    world({ rules: [] }, 3, false);
    open();
    expect(await screen.findByText("This project has no automations document to edit.")).toBeTruthy();
  });
});
