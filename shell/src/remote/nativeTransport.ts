import { Channel, invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { StepUpOutcome, Transport } from "../data/transport";

export interface NativeMachine {
  id: string;
  origin: string;
}

/** What the Rust side sends down the channel; one `end` or `error` always closes it. */
type FetchEvent = { event: "chunk"; data: number[] } | { event: "end" } | { event: "error"; data: string };

interface FetchHead {
  status: number;
  headers: [string, string][];
}

/** A response with one of these statuses cannot carry a body. */
const NO_BODY = new Set([101, 204, 205, 304]);

function abortError(): Error {
  return new DOMException("The request was aborted.", "AbortError");
}

function requestUrl(input: RequestInfo | URL): URL {
  return new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
}

/**
 * `fetch` for one paired machine, performed by the host. The session, the `Origin`,
 * the CSRF marker and the `Cookie` are all set in Rust; this side sends only a path,
 * a method, ordinary headers and a text body, and gets back a real `Response` whose
 * body streams, so JSON, SSE and downloads read exactly as they do from a browser.
 */
function machineFetch(machine: NativeMachine, origin: string): typeof fetch {
  return async (input, init) => {
    const url = requestUrl(input);
    if (url.origin !== origin) throw new Error("a paired machine only accepts requests to its own origin");
    if (init?.body !== undefined && init.body !== null && typeof init.body !== "string") {
      throw new Error("a request to a paired machine can only carry a text body");
    }
    const signal = init?.signal ?? undefined;
    if (signal?.aborted) throw abortError();

    const requestId = crypto.randomUUID();
    let controller!: ReadableStreamDefaultController<Uint8Array>;
    let finished = false;
    const finish = (error?: Error) => {
      if (finished) return;
      finished = true;
      signal?.removeEventListener("abort", onAbort);
      if (error === undefined) controller.close();
      else controller.error(error);
    };
    const cancelRemote = () => void invoke("machine_fetch_cancel", { requestId }).catch(() => undefined);
    const onAbort = () => {
      cancelRemote();
      finish(abortError());
    };
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
      },
      cancel() {
        if (!finished) {
          finished = true;
          signal?.removeEventListener("abort", onAbort);
          cancelRemote();
        }
      },
    });
    const channel = new Channel<FetchEvent>();
    channel.onmessage = (message) => {
      if (finished) return;
      if (message.event === "chunk") controller.enqueue(Uint8Array.from(message.data));
      else if (message.event === "end") finish();
      else finish(new Error(message.data));
    };
    signal?.addEventListener("abort", onAbort, { once: true });

    let head: FetchHead;
    try {
      head = await invoke<FetchHead>("machine_fetch", {
        id: machine.id,
        method: init?.method ?? "GET",
        path: url.pathname + url.search,
        headers: Object.fromEntries(new Headers(init?.headers).entries()),
        body: init?.body ?? null,
        onChunk: channel,
        requestId,
      });
    } catch (error) {
      signal?.removeEventListener("abort", onAbort);
      if (signal?.aborted) throw abortError();
      throw new Error(typeof error === "string" ? error : error instanceof Error ? error.message : "the machine did not answer");
    }
    if (NO_BODY.has(head.status)) {
      cancelRemote();
      finished = true;
      signal?.removeEventListener("abort", onAbort);
      return new Response(null, { status: head.status, headers: head.headers });
    }
    return new Response(body, { status: head.status, headers: head.headers });
  };
}

const STEP_UP_POLL_MS = 1_000;
const STEP_UP_WAIT_MS = 5 * 60_000;
const MOVE_CHAT_ACTION = "move_chat";

async function nativeStepUp(machine: NativeMachine, origin: string) {
  const fetchForMachine = machineFetch(machine, origin);
  try {
    const status = await fetchForMachine(`${origin}/api/stepup/status`);
    if (status.ok) {
      const body = (await status.json()) as { stepped_up?: unknown };
      if (body.stepped_up === true) return { kind: "verified" as const };
    }
    const created = await fetchForMachine(`${origin}/api/stepup/ticket`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Nucleos-Csrf": "1" },
      body: JSON.stringify({ action: MOVE_CHAT_ACTION }),
    });
    if (created.status === 400) {
      const body = (await created.json().catch(() => null)) as { refusal?: unknown } | null;
      const code = typeof body?.refusal === "string" ? body.refusal : "invalid_stepup_action";
      return { kind: "failed" as const, code, detail: "The step-up request was refused." };
    }
    if (!created.ok) throw new Error("ticket_refused");
    const ticket = (await created.json()) as { ticket?: unknown };
    if (typeof ticket.ticket !== "string" || ticket.ticket === "") throw new Error("ticket_refused");
    await openUrl(`${origin}/stepup?ticket=${encodeURIComponent(ticket.ticket)}`);
    const deadline = Date.now() + STEP_UP_WAIT_MS;
    while (Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, STEP_UP_POLL_MS));
      const polled = await fetchForMachine(`${origin}/api/stepup/status`);
      if (!polled.ok) break;
      const body = (await polled.json()) as { stepped_up?: unknown };
      if (body.stepped_up === true) return { kind: "verified" as const };
    }
  } catch {
    // The original grave refusal stays the only useful answer: opening a browser is best effort.
  }
  return { kind: "failed" as const, code: "step_up_required", detail: "Passkey confirmation was not completed. Nothing was done." };
}

/** Opens the fixed move-a-chat step-up flow for a paired machine. */
export async function requestNativeStepUp(machine: NativeMachine, _label = "Move chat"): Promise<StepUpOutcome> {
  return await nativeStepUp(machine, machine.origin.replace(/\/$/, ""));
}

/**
 * The installed shell talks to another gateway without a browser cookie jar and
 * without ever holding its session: every request is made by the host, from the
 * vault, to the one origin the machine was paired with.
 */
export function nativeTransport(machine: NativeMachine): Transport {
  const origin = machine.origin.replace(/\/$/, "");
  return {
    baseUrl: `${origin}/api`,
    credentials: "omit",
    fetch: machineFetch(machine, origin),
    async headers() {
      return {};
    },
    async onUnauthorized() {
      return "fail";
    },
    label: origin,
    unauthorizedCode: "session_invalid",
    probeHeaders: {},
    onStepUp: () => requestNativeStepUp(machine),
  };
}

export type MachineProbe = "ok" | "needs-pairing" | "unreachable";

/** A reachability probe deliberately uses the same native transport as data and SSE calls. */
export async function probeMachine(machine: NativeMachine): Promise<MachineProbe> {
  const origin = machine.origin.replace(/\/$/, "");
  // One hung gateway must not hold the caller's refresh loop: the probe has its own deadline.
  const signal = AbortSignal.timeout(10_000);
  try {
    const expired = new Promise<never>((_, reject) => {
      signal.addEventListener("abort", () => reject(abortError()), { once: true });
    });
    const sent = machineFetch(machine, origin)(`${origin}/auth/session`, { signal });
    sent.catch(() => undefined);
    const response = await Promise.race([sent, expired]);
    void response.body?.cancel();
    if (response.ok) return "ok";
    return response.status === 401 ? "needs-pairing" : "unreachable";
  } catch {
    return "unreachable";
  }
}
