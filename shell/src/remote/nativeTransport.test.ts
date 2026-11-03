import { afterEach, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const openUrl = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage: (message: unknown) => void = () => undefined;
  },
}));

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));

type MachineFetch = {
  id: string;
  path: string;
  body: string | null;
  onChunk: { onmessage: (message: unknown) => void };
};

function answer(fetch: MachineFetch, status: number, body: unknown): { status: number; headers: [string, string][] } {
  fetch.onChunk.onmessage({ event: "chunk", data: [...new TextEncoder().encode(JSON.stringify(body))] });
  fetch.onChunk.onmessage({ event: "end" });
  return { status, headers: [["content-type", "application/json"]] };
}

async function fresh() {
  vi.resetModules();
  const [{ nativeTransport }, { setTransport }, { apiFetch, ApiRefusal }] = await Promise.all([
    import("./nativeTransport"),
    import("../data/transport"),
    import("../data/client"),
  ]);
  setTransport(nativeTransport({ id: "tower", origin: "https://tower.example" }));
  return { apiFetch, ApiRefusal };
}

afterEach(() => {
  vi.useRealTimers();
  vi.clearAllMocks();
});

it("a_native_refusal_asks_for_step_up_in_the_browser_and_retries", async () => {
  vi.useFakeTimers();
  openUrl.mockResolvedValue(undefined);
  const paths: string[] = [];
  invoke.mockImplementation((command: string, args?: MachineFetch) => {
    if (command === "machine_fetch_cancel") return undefined;
    if (command !== "machine_fetch" || args === undefined) return undefined;
    paths.push(args.path);
    if (args.path === "/api/assistant/grave") {
      const attempts = paths.filter((path) => path === args.path).length;
      return answer(args, attempts === 1 ? 403 : 200, attempts === 1 ? { refusal: "step_up_required" } : { moved: true });
    }
    if (args.path === "/api/stepup/ticket") return answer(args, 201, { ticket: "one-time-ticket" });
    if (args.path === "/api/stepup/status") {
      const attempts = paths.filter((path) => path === args.path).length;
      return answer(args, 200, { stepped_up: attempts > 1 });
    }
    throw new Error(`unexpected path ${args.path}`);
  });
  const { apiFetch } = await fresh();

  const result = apiFetch<{ moved: boolean }>("/assistant/grave", { method: "POST", body: "{}" });
  await vi.advanceTimersByTimeAsync(1_000);

  await expect(result).resolves.toEqual({ moved: true });
  expect(openUrl).toHaveBeenCalledWith("https://tower.example/stepup?ticket=one-time-ticket");
  expect(paths).toEqual(["/api/assistant/grave", "/api/stepup/status", "/api/stepup/ticket", "/api/stepup/status", "/api/assistant/grave"]);
  expect(invoke.mock.calls.find(([, args]) => (args as MachineFetch | undefined)?.path === "/api/stepup/ticket")?.[1]).toMatchObject({ body: '{"action":"move_chat"}' });
});

it("an_existing_step_up_skips_ticket_creation", async () => {
  invoke.mockImplementation((command: string, args?: MachineFetch) => {
    if (command === "machine_fetch_cancel") return undefined;
    if (command !== "machine_fetch" || args === undefined) return undefined;
    if (args.path === "/api/assistant/grave") return answer(args, 403, { refusal: "step_up_required" });
    if (args.path === "/api/stepup/status") return answer(args, 200, { stepped_up: true });
    throw new Error(`unexpected path ${args.path}`);
  });
  const { apiFetch } = await fresh();

  await expect(apiFetch("/assistant/grave", { method: "POST", body: "{}" })).rejects.toMatchObject({ code: "step_up_required" });
  expect(openUrl).not.toHaveBeenCalled();
  expect(invoke.mock.calls.some(([, args]) => (args as MachineFetch | undefined)?.path === "/api/stepup/ticket")).toBe(false);
});

it("closing_the_browser_page_leaves_a_plain_refusal", async () => {
  vi.useFakeTimers();
  openUrl.mockResolvedValue(undefined);
  invoke.mockImplementation((command: string, args?: MachineFetch) => {
    if (command === "machine_fetch_cancel") return undefined;
    if (command !== "machine_fetch" || args === undefined) return undefined;
    if (args.path === "/api/assistant/grave") return answer(args, 403, { refusal: "step_up_required" });
    if (args.path === "/api/stepup/ticket") return answer(args, 201, { ticket: "one-time-ticket" });
    if (args.path === "/api/stepup/status") return answer(args, 200, { stepped_up: false });
    throw new Error(`unexpected path ${args.path}`);
  });
  const { apiFetch, ApiRefusal } = await fresh();

  const result = apiFetch("/assistant/grave", { method: "POST", body: "{}" }).catch((error: unknown) => error);
  await vi.advanceTimersByTimeAsync(5 * 60_000 + 1);

  const refusal = await result;
  expect(refusal).toBeInstanceOf(ApiRefusal);
  expect((refusal as InstanceType<typeof ApiRefusal>).code).toBe("step_up_required");
  expect(openUrl).toHaveBeenCalledOnce();
});

it("a_ticket_action_refusal_is_reported_without_the_generic_completion_message", async () => {
  invoke.mockImplementation((command: string, args?: MachineFetch) => {
    if (command === "machine_fetch_cancel") return undefined;
    if (command !== "machine_fetch" || args === undefined) return undefined;
    if (args.path === "/api/assistant/grave") return answer(args, 403, { refusal: "step_up_required" });
    if (args.path === "/api/stepup/status") return answer(args, 200, { stepped_up: false });
    if (args.path === "/api/stepup/ticket") return answer(args, 400, { refusal: "invalid_stepup_action" });
    throw new Error(`unexpected path ${args.path}`);
  });
  const { apiFetch, ApiRefusal } = await fresh();

  const refusal = await apiFetch("/assistant/grave", { method: "POST", body: "{}" }).catch((error: unknown) => error);
  expect(refusal).toBeInstanceOf(ApiRefusal);
  expect((refusal as InstanceType<typeof ApiRefusal>).code).toBe("invalid_stepup_action");
});
