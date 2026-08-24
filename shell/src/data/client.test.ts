import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Hoisted so the module factory below and the assertions down here hold the
// same mock instance — `vi.mock` is lifted above the imports, and a plain
// `const` declared here would not exist yet when the factory runs.
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

type ClientModule = typeof import("./client");

/**
 * A client with an empty memory.
 *
 * The token promise is module-level state — that is the whole point of it — so
 * a test about how many times the credential is read has to start from a
 * module that has never read it. `resetModules` is the only way to get one.
 */
async function freshClient(): Promise<ClientModule> {
  vi.resetModules();
  return await import("./client");
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

function textResponse(status: number, body: string, statusText = ""): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    statusText,
    text: async () => body,
    json: async () => {
      throw new SyntaxError("not JSON");
    },
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

describe("apiFetch refusals", () => {
  it("carries the daemon's own name for a refusal it named", async () => {
    const { apiFetch, ApiRefusal, isApiRefusal } = await freshClient();
    fetchMock.mockResolvedValue(jsonResponse(423, { refusal: "kill_switch" }));

    const error = await apiFetch("/assistant/message", { method: "POST", body: "{}" }).catch(
      (e: unknown) => e,
    );

    expect(error).toBeInstanceOf(ApiRefusal);
    expect(isApiRefusal(error)).toBe(true);
    const refusal = error as InstanceType<ClientModule["ApiRefusal"]>;
    expect(refusal.status).toBe(423);
    // The name, not a derivation of 423 — a page switching on `code` gets to
    // say "the kill switch is engaged" rather than "locked".
    expect(refusal.code).toBe("kill_switch");
  });

  it("keeps the name AND the sentence when a route sends both", async () => {
    const { apiFetch } = await freshClient();
    // How `POST /projects/{id}/write` refuses YAML the daemon could not read.
    fetchMock.mockResolvedValue(
      jsonResponse(422, { refusal: "invalid", detail: "unknown field `gate_commmand` at line 1 column 1" }),
    );

    const error = (await apiFetch("/projects/alpha/write", { method: "POST", body: "{}" }).catch(
      (e: unknown) => e,
    )) as InstanceType<ClientModule["ApiRefusal"]>;

    // The name a page switches on, and the sentence it shows. Collapsing the two
    // would leave an editor able to say only "invalid" about a file whose exact
    // broken line the daemon already located.
    expect(error.code).toBe("invalid");
    expect(error.detail).toContain("gate_commmand");
  });

  it("falls back to the name as the sentence when the route sent only a name", async () => {
    const { apiFetch } = await freshClient();
    fetchMock.mockResolvedValue(jsonResponse(403, { refusal: "not_ours" }));

    const error = (await apiFetch("/projects/alpha/write", { method: "POST", body: "{}" }).catch(
      (e: unknown) => e,
    )) as InstanceType<ClientModule["ApiRefusal"]>;

    expect(error.code).toBe("not_ours");
    expect(error.detail).toBe("not_ours");
  });

  it("derives a stable code from the status when the route refused in prose", async () => {
    const { apiFetch, ApiRefusal } = await freshClient();
    // How `POST /jobs` refuses a full project: (StatusCode, String), no JSON at all.
    fetchMock.mockResolvedValue(
      textResponse(409, "the kill switch is engaged; nothing autonomous starts"),
    );

    const error = (await apiFetch("/jobs", { method: "POST", body: "{}" }).catch(
      (e: unknown) => e,
    )) as InstanceType<ClientModule["ApiRefusal"]>;

    expect(error).toBeInstanceOf(ApiRefusal);
    expect(error.status).toBe(409);
    expect(error.code).toBe("conflict");
    expect(error.detail).toBe("the kill switch is engaged; nothing autonomous starts");
  });

  it("reads the sentence out of an {error} body and still codes it by status", async () => {
    const { apiFetch } = await freshClient();
    fetchMock.mockResolvedValue(jsonResponse(429, { error: "the window budget is spent" }));

    const error = (await apiFetch("/council").catch((e: unknown) => e)) as InstanceType<
      ClientModule["ApiRefusal"]
    >;

    expect(error.status).toBe(429);
    // Not "budget": only the route knows what its 429 meant, and guessing here
    // would put a wrong name on every other route's 429.
    expect(error.code).toBe("too_many_requests");
    expect(error.detail).toBe("the window budget is spent");
  });

  it("survives a refusal with no body at all", async () => {
    const { apiFetch, ApiRefusal } = await freshClient();
    fetchMock.mockResolvedValue(textResponse(404, "", "Not Found"));

    const error = (await apiFetch("/runs/99").catch((e: unknown) => e)) as InstanceType<
      ClientModule["ApiRefusal"]
    >;

    expect(error).toBeInstanceOf(ApiRefusal);
    expect(error.code).toBe("not_found");
  });
});

describe("apiFetch transport", () => {
  it("does not dress a dead daemon up as a refusal", async () => {
    const { apiFetch, ApiRefusal, ApiUnavailable, isApiRefusal } = await freshClient();
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"));

    const error = await apiFetch("/projects").catch((e: unknown) => e);

    // The distinction the whole error design exists for: a refusal is settled
    // and retrying is pointless; an absence clears by waiting.
    expect(error).not.toBeInstanceOf(ApiRefusal);
    expect(isApiRefusal(error)).toBe(false);
    expect(error).toBeInstanceOf(ApiUnavailable);
    expect((error as InstanceType<ClientModule["ApiUnavailable"]>).kind).toBe("transport");
  });

  it("tells a locked keychain apart from a dead daemon", async () => {
    const { apiFetch, ApiUnavailable } = await freshClient();
    invoke.mockRejectedValue("the credential store is locked");

    const error = await apiFetch("/projects").catch((e: unknown) => e);

    expect(error).toBeInstanceOf(ApiUnavailable);
    expect((error as InstanceType<ClientModule["ApiUnavailable"]>).kind).toBe("token");
    // Never reached the network — there was nothing to ask with.
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("returns nothing, rather than parsing nothing, on a 204", async () => {
    const { apiFetch } = await freshClient();
    // `POST /autopilot/kill` and `POST /autopilot/attention` both answer this
    // way; `json()` on the stub throws, exactly as it would on a real empty body.
    fetchMock.mockResolvedValue(textResponse(204, ""));

    await expect(
      apiFetch<void>("/autopilot/kill", { method: "POST", body: "{}" }),
    ).resolves.toBeUndefined();
  });
});

describe("the daemon token", () => {
  it("is read once and reused across every call", async () => {
    const { apiFetch } = await freshClient();
    fetchMock.mockResolvedValue(jsonResponse(200, []));

    await Promise.all([apiFetch("/projects"), apiFetch("/proposals"), apiFetch("/autopilot/kill")]);
    await apiFetch("/projects");

    // Four calls, one keychain read — the token rotates on daemon restart, not
    // per request, so re-reading it would be a credential-manager hit per poll tick.
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("get_daemon_token");
    expect(fetchMock).toHaveBeenCalledTimes(4);

    for (const call of fetchMock.mock.calls) {
      const init = call[1] as RequestInit;
      expect(new Headers(init.headers).get("Authorization")).toBe("Bearer token-abc");
    }
  });

  it("is retried after a failed read, so a momentary lock is not permanent", async () => {
    const { apiFetch } = await freshClient();
    fetchMock.mockResolvedValue(jsonResponse(200, []));
    invoke.mockRejectedValueOnce("locked").mockResolvedValue("token-abc");

    await expect(apiFetch("/projects")).rejects.toBeDefined();
    await expect(apiFetch("/projects")).resolves.toEqual([]);

    expect(invoke).toHaveBeenCalledTimes(2);
  });
});

describe("probeHealth", () => {
  it("answers false instead of throwing when the daemon is not there", async () => {
    const { probeHealth } = await freshClient();
    fetchMock.mockRejectedValue(new TypeError("Failed to fetch"));

    // The connection gate polls this forever; "not there" is an expected
    // reading during startup, not an error state to retry out of.
    await expect(probeHealth()).resolves.toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });
});
