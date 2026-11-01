(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { fireEvent, screen } from "@testing-library/react";
import { ApiRefusal, apiFetch } from "../data/client";
import { desktopTransport, setTransport } from "../data/transport";
import { PasskeyEnrol, credentialJson } from "./StepUp";
import { remoteTransport } from "./transport";

vi.mock("@tauri-apps/api/core", () => ({ invoke: async () => "token" }));

const GRAVE = "/api/settings/danger";

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

const STEP_UP_REQUIRED = () => json(403, { refusal: "step_up_required" });
const REGISTERED = { owner: true, registered: true, stepped_up: false, can_register: false };

// An ArrayBuffer-bearing assertion, the way a browser hands it back.
function buf(...bytes: number[]): ArrayBuffer {
  return new Uint8Array(bytes).buffer;
}

function assertion() {
  return {
    id: "cred",
    rawId: buf(1, 2),
    type: "public-key",
    response: {
      authenticatorData: buf(3),
      clientDataJSON: buf(4),
      signature: buf(5),
      userHandle: null,
    },
  };
}

const REQUEST_OPTIONS = { publicKey: { challenge: "AQID", rpId: "tower.example", allowCredentials: [{ type: "public-key", id: "AQI" }] } };
const CREATE_OPTIONS = {
  publicKey: {
    challenge: "AQID",
    rp: { id: "tower.example", name: "NucleOS" },
    user: { id: "BAU", name: "laptop", displayName: "laptop" },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [],
  },
};

interface Rig {
  api: ReturnType<typeof vi.fn>;
  paths: string[];
}

/** Routes by "METHOD path"; a route is a list of responses used in order (the last repeats). */
function rig(routes: Record<string, Array<() => Response>>): Rig {
  const used: Record<string, number> = {};
  const paths: string[] = [];
  const api = vi.fn(async (url: string, init?: RequestInit) => {
    const key = `${init?.method ?? "GET"} ${url}`;
    paths.push(key);
    const list = routes[key];
    if (list === undefined) throw new Error(`unexpected fetch ${key}`);
    const i = Math.min(used[key] ?? 0, list.length - 1);
    used[key] = (used[key] ?? 0) + 1;
    return list[i]!();
  });
  vi.stubGlobal("fetch", api);
  return { api, paths };
}

function count(paths: string[], key: string): number {
  return paths.filter((p) => p === key).length;
}

function stubCredentials(impl: { get?: () => Promise<unknown>; create?: () => Promise<unknown> } | undefined) {
  vi.stubGlobal("navigator", { ...navigator, credentials: impl });
}

/** Waits for the dialog's button to be ready (options fetched), then clicks it as a person would. */
async function clickDialog(name: string) {
  const button = await screen.findByRole("button", { name });
  await vi.waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  await act(async () => {
    fireEvent.click(button);
  });
}

const VERIFY_ROUTES = {
  "GET /auth/passkey/status": [() => json(200, REGISTERED)],
  "POST /auth/passkey/verify/begin": [() => json(200, REQUEST_OPTIONS)],
  "POST /auth/passkey/verify/finish": [() => json(200, { stepped_up: true, window_seconds: 300 })],
};

beforeEach(() => setTransport(remoteTransport));
afterEach(() => {
  setTransport(desktopTransport);
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

async function refusalOf(promise: Promise<unknown>): Promise<ApiRefusal> {
  try {
    await promise;
  } catch (error) {
    expect(error).toBeInstanceOf(ApiRefusal);
    return error as ApiRefusal;
  }
  throw new Error("expected a refusal");
}

describe("step-up in the remote transport", () => {
  it("a_step_up_refusal_prompts_and_retries_once", async () => {
    const get = vi.fn(async () => assertion());
    stubCredentials({ get });
    const { paths } = rig({
      ...VERIFY_ROUTES,
      [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, () => json(200, { done: true })],
    });

    const pending = apiFetch<{ done: boolean }>("/settings/danger", { method: "POST", body: "{}" });
    await clickDialog("Confirm with passkey");
    const result = await pending;

    expect(result.done).toBe(true);
    expect(get).toHaveBeenCalledTimes(1);
    expect(count(paths, `POST ${GRAVE}`)).toBe(2);
    // The grave request is repeated only after the passkey verified.
    expect(paths.indexOf("POST /auth/passkey/verify/finish")).toBeLessThan(paths.lastIndexOf(`POST ${GRAVE}`));
  });

  it("a second refusal after the retry is a failure and does not loop", async () => {
    const get = vi.fn(async () => assertion());
    stubCredentials({ get });
    const { paths } = rig({ ...VERIFY_ROUTES, [`POST ${GRAVE}`]: [STEP_UP_REQUIRED] });

    const outcome = refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));
    await clickDialog("Confirm with passkey");
    const refusal = await outcome;

    expect(refusal.code).toBe("step_up_required");
    expect(count(paths, `POST ${GRAVE}`)).toBe(2);
    expect(get).toHaveBeenCalledTimes(1);
  });

  it("a_refused_passkey_leaves_the_action_undone", async () => {
    const get = vi.fn(async () => {
      throw new DOMException("cancelled", "NotAllowedError");
    });
    stubCredentials({ get });
    const { paths } = rig({ ...VERIFY_ROUTES, [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, () => json(200, {})] });

    const outcome = refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));
    await clickDialog("Confirm with passkey");
    const refusal = await outcome;

    expect(refusal.status).toBe(403);
    expect(refusal.detail).toMatch(/cancel|not confirmed/i);
    expect(count(paths, `POST ${GRAVE}`)).toBe(1);
    expect(count(paths, "POST /auth/passkey/verify/finish")).toBe(0);
  });

  it("a gateway that refuses the assertion leaves the action undone", async () => {
    stubCredentials({ get: async () => assertion() });
    const { paths } = rig({
      ...VERIFY_ROUTES,
      "POST /auth/passkey/verify/finish": [() => json(400, { error: "passkey_refused" })],
      [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, () => json(200, {})],
    });

    const outcome = refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));
    await clickDialog("Confirm with passkey");
    const refusal = await outcome;

    expect(refusal.detail).toMatch(/passkey/i);
    expect(count(paths, `POST ${GRAVE}`)).toBe(1);
  });

  it("concurrent refusals share one prompt", async () => {
    let release: (value: unknown) => void = () => {};
    const get = vi.fn(() => new Promise((resolve) => (release = resolve)));
    stubCredentials({ get });
    const { paths } = rig({
      ...VERIFY_ROUTES,
      "POST /api/a": [STEP_UP_REQUIRED, () => json(200, { ok: "a" })],
      "POST /api/b": [STEP_UP_REQUIRED, () => json(200, { ok: "b" })],
    });

    const a = apiFetch<{ ok: string }>("/a", { method: "POST", body: "{}" });
    const b = apiFetch<{ ok: string }>("/b", { method: "POST", body: "{}" });
    await clickDialog("Confirm with passkey");
    await vi.waitFor(() => expect(get).toHaveBeenCalled());
    release(assertion());

    expect((await a).ok).toBe("a");
    expect((await b).ok).toBe("b");
    expect(get).toHaveBeenCalledTimes(1);
    expect(count(paths, "POST /auth/passkey/verify/begin")).toBe(1);
  });

  it("the_ceremony_runs_only_from_a_click", async () => {
    const get = vi.fn(async () => assertion());
    stubCredentials({ get });
    const { paths } = rig({ ...VERIFY_ROUTES, [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, () => json(200, { done: true })] });

    const pending = apiFetch<{ done: boolean }>("/settings/danger", { method: "POST", body: "{}" });
    const button = await screen.findByRole("button", { name: "Confirm with passkey" });
    // The options are already fetched, and the authenticator has still not been touched.
    await vi.waitFor(() => expect(count(paths, "POST /auth/passkey/verify/begin")).toBe(1));
    expect(get).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();

    await act(async () => {
      fireEvent.click(button);
      // Reached inside the click handler, with no await in between.
      expect(get).toHaveBeenCalledTimes(1);
    });
    expect((await pending).done).toBe(true);
  });

  it("cancelling the dialog leaves the action undone and the next attempt prompts again", async () => {
    const get = vi.fn(async () => assertion());
    stubCredentials({ get });
    const { paths } = rig({ ...VERIFY_ROUTES, [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, STEP_UP_REQUIRED] });

    const first = refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));
    await clickDialog("Cancel");
    expect((await first).detail).toMatch(/not confirmed/i);
    expect(get).not.toHaveBeenCalled();
    expect(count(paths, `POST ${GRAVE}`)).toBe(1);

    const second = refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));
    await screen.findByRole("button", { name: "Confirm with passkey" });
    await clickDialog("Cancel");
    await second;
    expect(count(paths, "GET /auth/passkey/status")).toBe(2);
  });

  it("enrolling_never_chains_into_a_confirmation_without_a_second_click", async () => {
    const create = vi.fn(async (_options: unknown) => ({
      id: "cred",
      rawId: buf(1),
      type: "public-key",
      response: { attestationObject: buf(2), clientDataJSON: buf(3) },
    }));
    const get = vi.fn(async () => assertion());
    stubCredentials({ create, get } as never);
    const { paths } = rig({
      "GET /auth/passkey/status": [() => json(200, { owner: true, registered: false, stepped_up: false, can_register: true })],
      "POST /auth/passkey/register/begin": [() => json(200, CREATE_OPTIONS)],
      "POST /auth/passkey/register/finish": [() => json(200, { registered: true })],
      "POST /auth/passkey/verify/begin": [() => json(200, REQUEST_OPTIONS)],
      "POST /auth/passkey/verify/finish": [() => json(200, { stepped_up: true, window_seconds: 300 })],
      [`POST ${GRAVE}`]: [STEP_UP_REQUIRED, () => json(200, { done: true })],
    });

    const pending = apiFetch<{ done: boolean }>("/settings/danger", { method: "POST", body: "{}" });
    await clickDialog("Register a passkey");
    expect(create).toHaveBeenCalledTimes(1);

    // Registered; the dialog now asks for a confirmation and waits for a click of its own.
    await screen.findByRole("button", { name: "Confirm with passkey" });
    expect(get).not.toHaveBeenCalled();
    expect(count(paths, "POST /auth/passkey/verify/finish")).toBe(0);

    await clickDialog("Confirm with passkey");
    expect(get).toHaveBeenCalledTimes(1);
    expect((await pending).done).toBe(true);
  });

  it("a device with no passkey and no way to enrol is not prompted and is sent to the main machine", async () => {
    const get = vi.fn();
    stubCredentials({ get });
    const { paths } = rig({
      "GET /auth/passkey/status": [() => json(200, { owner: false, registered: false, stepped_up: false, can_register: false })],
      [`POST ${GRAVE}`]: [STEP_UP_REQUIRED],
    });

    const refusal = await refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));

    expect(refusal.detail).toMatch(/main machine/);
    expect(get).not.toHaveBeenCalled();
    expect(count(paths, `POST ${GRAVE}`)).toBe(1);
    expect(count(paths, "POST /auth/passkey/verify/begin")).toBe(0);
  });

  it("an unavailable navigator.credentials leaves the action undone with a clear message", async () => {
    stubCredentials(undefined);
    const { paths } = rig({ ...VERIFY_ROUTES, [`POST ${GRAVE}`]: [STEP_UP_REQUIRED] });

    const refusal = await refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));

    expect(refusal.detail).toMatch(/does not support passkeys|secure connection/i);
    expect(count(paths, `POST ${GRAVE}`)).toBe(1);
  });

  it("the desktop transport is never asked for a passkey", async () => {
    setTransport(desktopTransport);
    const get = vi.fn();
    stubCredentials({ get });
    const { paths } = rig({ "POST http://127.0.0.1:8791/settings/danger": [STEP_UP_REQUIRED] });

    const refusal = await refusalOf(apiFetch("/settings/danger", { method: "POST", body: "{}" }));

    expect(refusal.code).toBe("step_up_required");
    expect(get).not.toHaveBeenCalled();
    expect(paths.some((p) => p.includes("/auth/"))).toBe(false);
  });
});

describe("the first passkey after an owner pairs", () => {
  function mount(onDone: () => void) {
    const container = document.createElement("div");
    document.body.appendChild(container);
    act(() => createRoot(container).render(<PasskeyEnrol onDone={onDone} />));
  }

  it("registers a passkey without restricting the authenticator, then carries on", async () => {
    const create = vi.fn(async (_options: unknown) => ({
      id: "cred",
      rawId: buf(1),
      type: "public-key",
      response: { attestationObject: buf(2), clientDataJSON: buf(3) },
    }));
    stubCredentials({ create });
    rig({
      "POST /auth/passkey/register/begin": [() => json(200, CREATE_OPTIONS)],
      "POST /auth/passkey/register/finish": [() => json(200, { registered: true })],
    });
    const onDone = vi.fn();
    mount(onDone);

    await clickDialog("Register a passkey");

    await vi.waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    const options = create.mock.calls[0]![0] as { publicKey: { authenticatorSelection?: { authenticatorAttachment?: string } } };
    expect(options.publicKey.authenticatorSelection?.authenticatorAttachment).toBeUndefined();
  });

  it("can be skipped, and says what that costs", async () => {
    stubCredentials({ create: vi.fn() });
    rig({ "POST /auth/passkey/register/begin": [() => json(200, CREATE_OPTIONS)] });
    const onDone = vi.fn();
    mount(onDone);

    expect(screen.getByText(/main machine/)).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));
    });

    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it("a closed first-passkey window says to make a new owner pairing code", async () => {
    stubCredentials({ create: vi.fn() });
    rig({ "POST /auth/passkey/register/begin": [() => json(403, { error: "pairing_window_closed" })] });
    const onDone = vi.fn();
    mount(onDone);

    await clickDialog("Register a passkey");

    const note = await screen.findByRole("alert");
    expect(note.textContent).toMatch(/15 minutes/);
    expect(note.textContent).toMatch(/new owner pairing code/);
    expect(onDone).not.toHaveBeenCalled();
  });

  it("a cancelled registration keeps the page and allows another try or a skip", async () => {
    stubCredentials({
      create: async () => {
        throw new DOMException("no", "NotAllowedError");
      },
    });
    rig({ "POST /auth/passkey/register/begin": [() => json(200, CREATE_OPTIONS)] });
    const onDone = vi.fn();
    mount(onDone);

    await clickDialog("Register a passkey");

    expect((await screen.findByRole("alert")).textContent).toMatch(/cancel/i);
    expect(onDone).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Skip for now" })).toBeTruthy();
  });
});

describe("credentialJson", () => {
  it("the_fallback_reads_prototype_getters", () => {
    class Answer {
      get clientDataJSON() {
        return buf(1, 2);
      }
      get authenticatorData() {
        return buf(3);
      }
      get signature() {
        return new Uint8Array([4, 5]);
      }
      get userHandle() {
        return null;
      }
    }
    const credential = { id: "cred", rawId: buf(9), type: "public-key", response: new Answer() };

    const json = credentialJson(credential as unknown as PublicKeyCredential) as {
      rawId: string;
      response: Record<string, string>;
    };

    expect(json.rawId).toBe("CQ");
    expect(json.response).toEqual({ clientDataJSON: "AQI", authenticatorData: "Aw", signature: "BAU" });
  });
});
