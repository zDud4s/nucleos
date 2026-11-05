// @vitest-environment jsdom
import type { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../remote/nativeTransport", () => ({
  probeMachine: vi.fn(async () => "ok"),
  nativeTransport: vi.fn(),
}));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { MachinePicker, MachineTag, HERE_WARNING, useNewWorkMachine } from "../chats/MachinePicker";
import { chatMachineOf, machinesForProject, mergeChats, rememberChatMachine } from "../data/chatMachines";
import {
  liveTurnKey,
  useAllChats,
  useArchiveChat,
  useChatTranscript,
  useForwardTurn,
  useLiveTurnSnapshot,
  useSendMessage,
  useSetChatGroup,
  useTurnTools,
  type ChatSummary,
} from "../data/chats";
import { ChangeProject } from "./Chats";
import { keys } from "../data/keys";
import { forgetMachine, withMachine } from "../data/machines";

const TOWER = { id: "tower", name: "Tower", origin: "https://tower.example" };

function pairTower() {
  localStorage.setItem("nucleos-machines", JSON.stringify([TOWER]));
}

function chat(id: string, last: string): ChatSummary {
  return { chat_id: id, title: id, last_activity: last, waiting: 0 } as unknown as ChatSummary;
}

function wrapper(client: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
}

function newClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
}

function Harness({ project }: { project?: { machine: string; remote: string | null } }) {
  const [machine, setMachine] = useNewWorkMachine();
  return <MachinePicker value={machine} onChange={setMachine} project={project} />;
}

beforeEach(() => {
  cleanup();
  localStorage.clear();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue([]);
});

describe("Chats - choosing where work executes", () => {
  it("new_work_defaults_to_the_primary_machine", async () => {
    pairTower();
    localStorage.setItem("nucleos-default-machine", "tower");
    render(<Harness />, { wrapper: wrapper(newClient()) });
    const select = (await screen.findByLabelText("Where it runs")) as HTMLSelectElement;
    expect(select.value).toBe("tower");
    expect(screen.queryByText(HERE_WARNING)).toBeNull();
  });

  it("choosing_this_device_shows_the_warning", async () => {
    pairTower();
    localStorage.setItem("nucleos-default-machine", "tower");
    render(<Harness />, { wrapper: wrapper(newClient()) });
    fireEvent.change(await screen.findByLabelText("Where it runs"), { target: { value: "here" } });
    expect(
      screen.getByText("Runs on this device. It stops when this device sleeps or shuts down."),
    ).toBeTruthy();
  });

  it("a_single_install_shows_no_picker", () => {
    const { container } = render(<Harness />, { wrapper: wrapper(newClient()) });
    expect(container.innerHTML).toBe("");
  });

  it("a_machine_is_not_offered_for_a_project_it_does_not_have", async () => {
    pairTower();
    daemon.apiFetch.mockResolvedValue([{ remote: "git@host:team/other.git" }]);
    const { unmount } = render(<MachinePicker value="here" onChange={() => {}} project={{ machine: "here", remote: "git@host:team/app.git" }} />, {
      wrapper: wrapper(newClient()),
    });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/projects", undefined, "tower"));
    expect(screen.queryByRole("option", { name: /Tower/ })).toBeNull();
    unmount();

    // A project with no remote exists only on its own machine, whatever the tower holds.
    render(<MachinePicker value="here" onChange={() => {}} project={{ machine: "here", remote: null }} />, {
      wrapper: wrapper(newClient()),
    });
    expect(screen.queryByRole("option", { name: /Tower/ })).toBeNull();
    cleanup();

    // The same remote on the tower: now it is offered.
    daemon.apiFetch.mockResolvedValue([{ remote: "git@host:team/app.git" }]);
    render(<MachinePicker value="here" onChange={() => {}} project={{ machine: "here", remote: "git@host:team/app.git" }} />, {
      wrapper: wrapper(newClient()),
    });
    expect(await screen.findByRole("option", { name: /Tower/ })).toBeTruthy();
  });
});

describe("Chats - the list across machines", () => {
  it("the_list_shows_which_machine_each_chat_runs_on", () => {
    const rows = mergeChats([
      { machine: "here", name: "This device", chats: [chat("h-1", "2026-10-01T10:00:00Z")], failed: false },
      { machine: "tower", name: "Tower", chats: [chat("t-1", "2026-10-01T11:00:00Z")], failed: false },
    ]);
    expect(rows.map((row) => [row.chat_id, row.machine])).toEqual([
      ["t-1", "tower"],
      ["h-1", "here"],
    ]);
    render(<MachineTag row={rows[0]} />);
    expect(screen.getByText("Tower")).toBeTruthy();
    cleanup();
    // One machine, no name given: nothing is drawn.
    const single = mergeChats([{ machine: "here", chats: [chat("h-1", "x")], failed: false }]);
    const { container } = render(<MachineTag row={single[0]} />);
    expect(container.innerHTML).toBe("");
  });

  it("an_unreachable_machine_marks_its_chats_and_keeps_the_others_working", async () => {
    pairTower();
    const client = newClient();
    // The tower answered once, earlier: that result is what stays on screen when it stops.
    client.setQueryData(withMachine(keys.chats.all, "tower"), [chat("t-1", "2026-10-01T11:00:00Z")]);
    daemon.apiFetch.mockImplementation(async (path: string, _init?: unknown, machine?: string) => {
      if (machine === "tower") throw new Error("offline");
      return path === "/assistant/chats" ? [chat("h-1", "2026-10-01T10:00:00Z")] : [];
    });
    const { result } = renderHook(() => useAllChats(), { wrapper: wrapper(client) });
    await waitFor(() => {
      const rows = result.current.data ?? [];
      expect(rows.find((row) => row.chat_id === "t-1")?.unreachable).toBe(true);
      expect(rows.find((row) => row.chat_id === "h-1")?.unreachable).toBe(false);
    });
  });

  it("a_chat_stays_on_its_machine_after_a_reload", async () => {
    pairTower();
    // A fresh page: the link was written by an earlier session and only storage remembers it.
    localStorage.setItem("nucleos-chat-machines", JSON.stringify({ "c-9": "tower" }));
    daemon.apiFetch.mockResolvedValue({ turns: [] });
    renderHook(() => useChatTranscript("c-9"), { wrapper: wrapper(newClient()) });
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-9", expect.anything(), "tower"),
    );
    expect(daemon.apiFetch.mock.calls.every(([, , machine]) => machine === "tower")).toBe(true);
  });

  it("a_chat_opened_by_url_finds_its_machine_when_the_list_arrives", async () => {
    pairTower();
    daemon.apiFetch.mockImplementation(async (path: string, _init?: unknown, machine?: string) => {
      if (path === "/assistant/chats") return machine === "tower" ? [chat("c-9", "2026-10-01T11:00:00Z")] : [];
      // This machine has never heard of the chat: the unlinked request is a 404.
      if (machine !== "tower") throw new Error("404 not found");
      return { turns: [] };
    });
    const { result } = renderHook(
      () => {
        useAllChats();
        return useChatTranscript("c-9");
      },
      { wrapper: wrapper(newClient()) },
    );
    // Without a link the transcript is first asked of this machine; once the list records the
    // link, with nothing else re-rendering, the same chat is asked of the tower.
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-9", expect.anything(), "tower"),
    );
    expect(chatMachineOf("c-9")).toBe("tower");
    // The new key means a new entry: the local 404 does not stay, and the transcript shows.
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(result.current.isError).toBe(false);
  });

  it("a moved remote row does not overwrite the chat's live machine link", async () => {
    pairTower();
    rememberChatMachine(["c-9"], "here");
    const moved = { ...chat("c-9", "2026-10-01T11:00:00Z"), moved_to: "here" };
    daemon.apiFetch.mockImplementation(async (path: string, _init?: unknown, machine?: string) =>
      path === "/assistant/chats" ? (machine === "tower" ? [moved] : []) : [],
    );
    const client = newClient();
    const { result } = renderHook(() => useAllChats(), { wrapper: wrapper(client) });

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats", undefined, "tower"));
    await waitFor(() => {
      expect(client.getQueryData(withMachine(keys.chats.all, "tower"))).toEqual([moved]);
      expect(result.current.data).toEqual([]);
      expect(chatMachineOf("c-9")).toBe("here");
    });
  });

  it("a_remote_chats_tool_results_come_from_its_machine", async () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    daemon.apiFetch.mockResolvedValue({ did: [] });
    const client = newClient();
    renderHook(() => useTurnTools(5, true, "c-9"), { wrapper: wrapper(client) });
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/turns/5/tools", undefined, "tower"),
    );
    // The same bare turn id for a local chat is its own record, fetched from this machine.
    renderHook(() => useTurnTools(5, true, "c-1"), { wrapper: wrapper(client) });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/turns/5/tools"));
  });

  it("the_same_turn_id_on_two_machines_does_not_share_a_live_entry", () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    expect(liveTurnKey(5, "c-9")).not.toEqual(liveTurnKey(5, "c-1"));
    expect(liveTurnKey(5, "c-1")).toEqual(keys.chats.live(5));
    const client = newClient();
    client.setQueryData(liveTurnKey(5, "c-9"), { text: "from the tower" });
    const tower = renderHook(() => useLiveTurnSnapshot(5, "c-9"), { wrapper: wrapper(client) });
    const here = renderHook(() => useLiveTurnSnapshot(5, "c-1"), { wrapper: wrapper(client) });
    expect(tower.result.current).toMatchObject({ text: "from the tower" });
    expect(here.result.current).toBeUndefined();
  });

  it("forgetting_a_machine_clears_its_chat_links", async () => {
    pairTower();
    rememberChatMachine(["c-9", "c-8"], "tower");
    rememberChatMachine(["c-7"], "laptop");
    await forgetMachine("tower");
    expect(chatMachineOf("c-9")).toBe("here");
    expect(chatMachineOf("c-8")).toBe("here");
    expect(chatMachineOf("c-7")).toBe("laptop");
  });

  it("a_remote_chats_project_cannot_be_changed_from_here", () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    render(<ChangeProject chatId="c-9" picking onPicking={() => {}} />, { wrapper: wrapper(newClient()) });
    expect(
      screen.getByText(/This chat runs on another machine, so its project can only be changed there\./),
    ).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("the_same_repository_by_ssh_and_https_counts_as_the_same_project", () => {
    const held = [{ machine: "tower", projects: [{ remote: "https://GitHub.com/team/app" }] }];
    const project = { machine: "here", remote: "git@github.com:team/app.git" };
    expect(machinesForProject(project, ["here", "tower"], held)).toEqual(["here", "tower"]);
    const other = [{ machine: "tower", projects: [{ remote: "https://github.com/team/other.git/" }] }];
    expect(machinesForProject(project, ["here", "tower"], other)).toEqual(["here"]);
    const slash = [{ machine: "tower", projects: [{ remote: "https://github.com/team/app.git/" }] }];
    expect(machinesForProject(project, ["here", "tower"], slash)).toEqual(["here", "tower"]);
  });

  it("sending_in_a_chat_goes_to_that_chats_machine", async () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    expect(chatMachineOf("c-9")).toBe("tower");
    daemon.apiFetch.mockResolvedValue({ turn_id: 5 });
    const client = newClient();
    const remote = renderHook(() => useSendMessage("c-9"), { wrapper: wrapper(client) });
    remote.result.current.mutate({ text: "hello", images: [] });
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/assistant/message",
        expect.objectContaining({ method: "POST" }),
        "tower",
      ),
    );
    const local = renderHook(() => useSendMessage("c-1"), { wrapper: wrapper(client) });
    local.result.current.mutate({ text: "hello", images: [] });
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/message", expect.objectContaining({ method: "POST" })),
    );
  });

  it("remote group and forward mutations go to the chat's machine", async () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    daemon.apiFetch.mockResolvedValue({ turn_id: 5 });
    const client = newClient();
    const { result } = renderHook(() => ({ group: useSetChatGroup(), forward: useForwardTurn() }), {
      wrapper: wrapper(client),
    });

    result.current.group.mutate({ chatId: "c-9", groupId: 4 });
    result.current.forward.mutate({ toChatId: "c-9", fromTurnId: 3, text: "send this" });

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/assistant/chats/c-9/group",
        expect.objectContaining({ method: "PUT" }),
        "tower",
      ),
    );
    expect(daemon.apiFetch).toHaveBeenCalledWith(
      "/assistant/chats/c-9/forward",
      expect.objectContaining({ method: "POST" }),
      "tower",
    );
  });

  it("a remote archive invalidates that machine's chat list", async () => {
    pairTower();
    rememberChatMachine(["c-9"], "tower");
    daemon.apiFetch.mockResolvedValue(undefined);
    const client = newClient();
    const remoteKey = withMachine(keys.chats.all, "tower");
    client.setQueryData(remoteKey, [chat("c-9", "2026-10-01T11:00:00Z")]);
    const { result } = renderHook(() => useArchiveChat(), { wrapper: wrapper(client) });

    result.current.mutate("c-9");

    await waitFor(() => expect(client.getQueryState(remoteKey)?.isInvalidated).toBe(true));
  });
});
