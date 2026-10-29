(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient } from "@tanstack/react-query";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { emitSessionLost } from "../data/transport";
import { startRemoteApp } from "./RemoteApp";

const SESSION = { device_id: "d1", name: "Laptop", access_level: "full", expires_at: "2027-01-01T00:00:00Z" };

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers });
}

function setup(routes: Record<string, () => Response>) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const route = routes[url];
      if (route === undefined) throw new Error(`unexpected fetch ${url}`);
      return route();
    }),
  );
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  const queryClient = new QueryClient();
  const mountMain = vi.fn((r: ReturnType<typeof createRoot>) => {
    act(() => r.render(<p>the app</p>));
  });
  let stop = () => {};
  act(() => {
    stop = startRemoteApp(root, queryClient, mountMain);
  });
  return { root, queryClient, mountMain, stop: () => stop() };
}

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("the pairing gate", () => {
  it("a good code shows the app, a bad code shows the refusal", async () => {
    let accept = false;
    const { mountMain, stop } = setup({
      "/auth/session": () => json(401, { error: "session_invalid" }),
      "/auth/pair": () => (accept ? json(200, SESSION) : json(401, { error: "invalid_code" })),
    });

    const input = await screen.findByLabelText("Pairing code");
    fireEvent.change(input, { target: { value: "abcde fghij" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Pair" }));
    });
    expect((await screen.findByRole("alert")).textContent).toMatch(/not accepted/);
    expect(mountMain).not.toHaveBeenCalled();

    accept = true;
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Pair" }));
    });
    expect(await screen.findByText("the app")).toBeTruthy();
    expect(mountMain).toHaveBeenCalledTimes(1);

    const calls = vi.mocked(fetch).mock.calls;
    const body = JSON.parse((calls[calls.length - 1]?.[1] as RequestInit).body as string);
    expect(body.code).toBe("ABCDEFGHIJ");
    stop();
  });

  it("a session-lost signal returns to the pairing screen", async () => {
    const { queryClient, stop } = setup({ "/auth/session": () => json(200, SESSION) });
    expect(await screen.findByText("the app")).toBeTruthy();
    queryClient.setQueryData(["x"], 1);

    act(() => emitSessionLost());

    await waitFor(() => expect(screen.getByLabelText("Pairing code")).toBeTruthy());
    expect(queryClient.getQueryData(["x"])).toBeUndefined();
    stop();
  });
});
