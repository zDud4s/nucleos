import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";
import { LoadoutPreview } from "./LoadoutPreview";
import { renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

const ANSWER = {
  agent_id: "scout",
  team_id: null,
  box: "job_node",
  memory: "What you know:\n- Copy is short",
  block: "What you know:\n- Copy is short\n\nContext files you were given:\n- /x/gone.txt (file) (em falta)",
  tools: [
    { name: "read_context", origin: "base" },
    { name: "web_read", origin: "agent" },
  ],
  refs: [
    { owner_kind: "agent", path: "/x/here.txt", kind: "file", note: "the brief", state: "active" },
    { owner_kind: "team", path: "/x/gone.txt", kind: "file", note: null, state: "missing" },
  ],
  add_dirs: [],
};

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("LoadoutPreview", () => {
  it("asks for nothing until the owner previews", () => {
    renderWithQuery(<LoadoutPreview agentId="scout" />);
    expect(screen.getByRole("region", { name: "Loadout preview" })).toBeDefined();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("posts the agent, the box and the task, and shows memory, tools and refs", async () => {
    daemon.apiFetch.mockResolvedValue(ANSWER);
    renderWithQuery(<LoadoutPreview agentId="scout" />);

    fireEvent.change(screen.getByLabelText("Sample task"), {
      target: { value: "write the launch copy" },
    });
    fireEvent.click(screen.getByLabelText("Job node"));
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    const memory = await screen.findByRole("region", { name: "Memory it would receive" });
    expect(memory.textContent).toContain("Copy is short");

    const [path, init] = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/loadout/preview");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      agent_id: "scout",
      box: "job_node",
      task: "write the launch copy",
    });

    const tools = screen.getByRole("region", { name: "Tools it would hold" });
    expect(within(tools).getByText("web_read")).toBeDefined();
    expect(within(tools).getByText("approved for the agent")).toBeDefined();
    expect(within(tools).getByText("box base")).toBeDefined();

    const refs = screen.getByRole("region", { name: "Context it would be offered" });
    expect(within(refs).getByText("/x/here.txt")).toBeDefined();
    expect(within(refs).getByText("missing")).toBeDefined();
    expect(refs.textContent).toContain("the brief");
  });

  it("defaults to the team box and says when nothing would be shown", async () => {
    daemon.apiFetch.mockResolvedValue({
      ...ANSWER,
      box: "team",
      memory: null,
      block: "",
      tools: [],
      refs: [],
    });
    renderWithQuery(<LoadoutPreview agentId="scout" />);
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    expect(await screen.findByText("No memory would be shown for this task.")).toBeDefined();
    expect(screen.getByText("No tools: this agent would run without any.")).toBeDefined();
    expect(screen.getByText("No context files would be offered.")).toBeDefined();
    const [, init] = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body)).box).toBe("team");
  });

  it("says so when the núcleo refuses", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("404"));
    renderWithQuery(<LoadoutPreview agentId="scout" />);
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(await screen.findByRole("alert")).toBeDefined();
  });
});
