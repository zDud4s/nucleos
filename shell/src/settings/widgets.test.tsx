import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { cronProblem, widgetFor, widgetNamed } from "./widgets";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

const GROUPS = {
  groups: [
    {
      provider: "anthropic",
      family: "claude",
      label: "Claude · Opus",
      models: [{ id: "opus", label: "Opus", brain: "claude", efforts: [] }],
    },
  ],
  source: "fallback",
  catalogue_version: "1",
  fetched_at: null,
};

const SECRETS = {
  secrets: [{ key: "github-token", area: "github", what: "token", present: true }],
};

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation((path: string) => {
    if (path.startsWith("/assistant/models/groups")) return Promise.resolve(GROUPS);
    if (path === "/config/secrets") return Promise.resolve(SECRETS);
    return Promise.reject(new Error(`unexpected ${path}`));
  });
});

/** The one listed credential, with what the store said about it. */
function answerSecretWith(present: boolean | null) {
  daemon.apiFetch.mockImplementation((path: string) => {
    if (path === "/config/secrets") {
      return Promise.resolve({ secrets: [{ ...SECRETS.secrets[0], present }] });
    }
    return Promise.reject(new Error(`unexpected ${path}`));
  });
}

function Host({ widget, nullable = false, initial = "" }: { widget: string; nullable?: boolean; initial?: string }) {
  const [value, setValue] = useState(initial);
  const found = widgetNamed(widget);
  if (found === undefined) throw new Error(`no widget ${widget}`);
  const Control = found.Control;
  return (
    <div>
      <label htmlFor="w">the field</label>
      <Control id="w" value={value} onChange={setValue} nullable={nullable} />
      <output data-testid="value">{value}</output>
    </div>
  );
}

describe("widgetFor", () => {
  it("applies to a string node that names a known widget", () => {
    expect(widgetFor({ type: "string", "x-widget": "cron" })).not.toBeNull();
  });

  it("ignores a widget on a node that is not a string", () => {
    expect(widgetFor({ type: "array", items: { type: "string" }, "x-widget": "command" })).toBeNull();
  });

  it("ignores a name it does not know", () => {
    expect(widgetFor({ type: "string", "x-widget": "sparkles" })).toBeNull();
    expect(widgetFor({ type: "string" })).toBeNull();
  });
});

describe("cronProblem", () => {
  it("accepts an empty box and a five-field expression", () => {
    expect(cronProblem("")).toBeNull();
    expect(cronProblem("*/5 * * * *")).toBeNull();
    expect(cronProblem("0 9 * * 1-5")).toBeNull();
    expect(cronProblem("0,30 9-17 1 JAN MON")).toBeNull();
  });

  it("counts the fields", () => {
    expect(cronProblem("* * * *")).toBe(
      "Expected five fields (minute hour day-of-month month weekday), found 4.",
    );
  });

  it("names the field that is wrong", () => {
    expect(cronProblem("* * * * ?x")).toBe('"?x" is not a valid cron field.');
  });
});

describe("the model picker", () => {
  it("offers the catalogue grouped by family and writes the model id", async () => {
    renderWithQuery(<Host widget="model-picker" nullable />);
    const select = screen.getByLabelText("the field");
    await waitFor(() => { expect(within(select).getByRole("option", { name: "Opus" })).toBeDefined(); });
    fireEvent.change(select, { target: { value: "opus" } });
    expect(screen.getByTestId("value").textContent).toBe("opus");
  });

  it("keeps a value the catalogue does not list", async () => {
    renderWithQuery(<Host widget="model-picker" initial="retired-model" />);
    const select = screen.getByLabelText("the field");
    await waitFor(() => { expect(within(select).getByRole("option", { name: "Opus" })).toBeDefined(); });
    expect(within(select).getByRole("option", { name: "retired-model" })).toBeDefined();
  });
});

describe("the model picker routes", () => {
  it("model picker puts agy rows in an agy optgroup with the route in the label", async () => {
    const sonnet = { id: "claude-sonnet-4-6", label: "Sonnet 4.6", brain: "cloud", efforts: [] };
    const agySonnet = {
      id: "agy/claude-sonnet-4-6",
      label: "Sonnet 4.6",
      brain: "cloud",
      efforts: [],
      runner: "agy",
    };
    daemon.apiFetch.mockImplementation((path: string) => {
      if (path.startsWith("/assistant/models/groups")) {
        return Promise.resolve({
          // The daemon files an agy row by the hosted model's family, inside the Claude company.
          groups: [
            {
              provider: "anthropic",
              family: "sonnet",
              label: "Claude · Sonnet",
              models: [sonnet, agySonnet],
            },
          ],
          source: "fallback",
          catalogue_version: "1",
          fetched_at: null,
        });
      }
      return Promise.reject(new Error(`unexpected ${path}`));
    });

    renderWithQuery(<Host widget="model-picker" nullable />);
    const select = screen.getByLabelText("the field");
    await waitFor(() => {
      expect(within(select).getByRole("option", { name: "Sonnet 4.6 (agy CLI)" })).toBeDefined();
    });

    const agy = within(select).getByRole("group", { name: "Antigravity CLI" });
    expect(within(agy).getByRole("option", { name: "Sonnet 4.6 (agy CLI)" })).toBeDefined();
    // The company optgroup keeps only the house row.
    const claude = within(select).getByRole("group", { name: "Claude · Sonnet" });
    expect(within(claude).getByRole("option", { name: "Sonnet 4.6" })).toBeDefined();
    expect(within(claude).queryByRole("option", { name: /agy/ })).toBeNull();

    fireEvent.change(select, { target: { value: "agy/claude-sonnet-4-6" } });
    expect(screen.getByTestId("value").textContent).toBe("agy/claude-sonnet-4-6");
  });
});

describe("the cron box", () => {
  it("says what is wrong while the text is wrong, and writes it anyway", () => {
    renderWithQuery(<Host widget="cron" />);
    fireEvent.change(screen.getByLabelText("the field"), { target: { value: "* * *" } });
    expect(screen.getByText(/Expected five fields/)).toBeDefined();
    expect(screen.getByLabelText("the field").getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByTestId("value").textContent).toBe("* * *");
  });
});

describe("the command box", () => {
  it("is a plain, unspellchecked text box", () => {
    renderWithQuery(<Host widget="command" initial="make test" />);
    const box = screen.getByLabelText("the field");
    expect(box.getAttribute("spellcheck")).toBe("false");
    fireEvent.change(box, { target: { value: "go test ./..." } });
    expect(screen.getByTestId("value").textContent).toBe("go test ./...");
  });
});

describe("the credential reference", () => {
  it("lists the credential names this machine knows and says whether one is stored", async () => {
    renderWithQuery(<Host widget="secret-ref" initial="github-token" />);
    await waitFor(() => { expect(screen.getByText(/stored in the keyring/)).toBeDefined(); });
    expect(
      within(screen.getByLabelText("the field")).getByRole("option", { name: "github-token" }),
    ).toBeDefined();
  });

  it("says nothing is stored under a name that is not", async () => {
    renderWithQuery(<Host widget="secret-ref" initial="ghost" />);
    await waitFor(() => {
      expect(screen.getByText("Nothing is stored under this name yet.")).toBeDefined();
    });
  });

  it("says nothing is stored when the keyring answers no for a listed name", async () => {
    answerSecretWith(false);
    renderWithQuery(<Host widget="secret-ref" initial="github-token" />);
    await waitFor(() => {
      expect(screen.getByText("Nothing is stored under this name yet.")).toBeDefined();
    });
  });

  it("does not report a keyring that did not answer as a name with nothing stored", async () => {
    answerSecretWith(null);
    renderWithQuery(<Host widget="secret-ref" initial="github-token" />);
    await waitFor(() => {
      expect(screen.getByText("Could not check the keyring for this name.")).toBeDefined();
    });
    expect(screen.queryByText("Nothing is stored under this name yet.")).toBeNull();
    expect(screen.queryByText(/stored in the keyring/)).toBeNull();
  });
});
