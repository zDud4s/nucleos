import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

type ClientModule = typeof import("./client");
type TransportModule = typeof import("./transport");
type RemoteModule = typeof import("../remote/transport");

/** A client, its transport module and the remote transport, all with empty memory and the remote one active. */
async function remoteClient(): Promise<{ client: ClientModule; transport: TransportModule; remote: RemoteModule }> {
  vi.resetModules();
  const client = await import("./client");
  const transport = await import("./transport");
  const remote = await import("../remote/transport");
  transport.setTransport(remote.remoteTransport);
  return { client, transport, remote };
}

function jsonResponse(status: number, body: unknown): Response {
  const text = JSON.stringify(body);
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText: "",
    text: async () => text,
    json: async () => JSON.parse(text),
  } as unknown as Response;
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue("token-abc");
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("remote transport", () => {
  it("remote transport sends same-origin /api with no Authorization and the CSRF header", async () => {
    const { client } = await remoteClient();
    fetchMock.mockResolvedValue(jsonResponse(200, ["ok"]));

    await expect(client.apiFetch("/projects")).resolves.toEqual(["ok"]);

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/projects");
    expect(init.credentials).toBe("same-origin");
    const headers = new Headers(init.headers);
    expect(headers.get("X-Nucleos-Csrf")).toBe("1");
    expect(headers.has("Authorization")).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("remote 401 signals session lost and never re-reads a token", async () => {
    const { client, transport } = await remoteClient();
    const lost = vi.fn();
    transport.onSessionLost(lost);
    fetchMock.mockResolvedValue(jsonResponse(401, { error: "session_invalid" }));

    const error = await client.apiFetch("/projects").catch((e: unknown) => e);

    expect(client.isApiRefusal(error)).toBe(true);
    const refusal = error as InstanceType<ClientModule["ApiRefusal"]>;
    expect(refusal.status).toBe(401);
    expect(refusal.code).toBe("session_invalid");
    expect(lost).toHaveBeenCalledTimes(1);
    // No retry: the session is gone, there is nothing to re-read.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("refusals and unavailability keep their classes on the remote transport", async () => {
    const { client } = await remoteClient();

    fetchMock.mockResolvedValueOnce(jsonResponse(423, { refusal: "kill_switch" }));
    const refusal = await client.apiFetch("/assistant/message").catch((e: unknown) => e);
    expect(client.isApiRefusal(refusal)).toBe(true);
    expect((refusal as InstanceType<ClientModule["ApiRefusal"]>).code).toBe("kill_switch");

    fetchMock.mockRejectedValueOnce(new TypeError("offline"));
    const down = await client.apiFetch("/projects").catch((e: unknown) => e);
    expect(client.isApiUnavailable(down)).toBe(true);
    expect((down as InstanceType<ClientModule["ApiUnavailable"]>).kind).toBe("transport");
  });
});
