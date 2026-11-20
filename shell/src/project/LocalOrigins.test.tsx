import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderWithQuery } from "../test/harness";
import { settingsDouble } from "../test/settings-double";
import { LocalOrigins, parseLocalOrigin } from "./LocalOrigins";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function world(body: unknown = {}, version = 3) {
  const double = settingsDouble([
    { scope: "project:alpha", name: "browser", schema: { type: "object" }, body, version },
  ]);
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => double.handle(path, init));
  return double;
}

const open = () => renderWithQuery(<LocalOrigins projectId="alpha" />);
const type = (text: string) => fireEvent.change(screen.getByLabelText("New origin"), { target: { value: text } });
const add = () => fireEvent.click(screen.getByRole("button", { name: "Add" }));

describe("LocalOrigins", () => {
  it("renders the stored origins", async () => {
    world({ local_origins: ["http://localhost:5173"] });
    open();
    expect(await screen.findByText("http://localhost:5173")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove http://localhost:5173" })).toBeTruthy();
  });

  it("adds an origin with a PUT of the whole document at the version read", async () => {
    const double = world({ other: 1, local_origins: ["http://localhost:5173"] });
    open();
    await screen.findByText("http://localhost:5173");
    type("http://127.0.0.1:3000");
    add();
    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0]).toEqual({
      scope: "project:alpha",
      name: "browser",
      version: 3,
      body: { other: 1, local_origins: ["http://localhost:5173", "http://127.0.0.1:3000"] },
    });
  });

  it("removes an origin", async () => {
    const double = world({ local_origins: ["http://localhost:5173", "http://localhost:8080"] });
    open();
    await screen.findByText("http://localhost:8080");
    fireEvent.click(screen.getByRole("button", { name: "Remove http://localhost:5173" }));
    await waitFor(() => expect(double.writes.length).toBe(1));
    expect(double.writes[0].body).toEqual({ local_origins: ["http://localhost:8080"] });
  });

  it("rejects an invalid origin without writing", async () => {
    const double = world();
    open();
    await screen.findByLabelText("New origin");
    for (const bad of ["http://example.com:80", "http://localhost", "http://localhost:5173/app", "ftp://localhost:1"]) {
      type(bad);
      add();
      expect(await screen.findByRole("alert")).toBeTruthy();
    }
    expect(double.writes.length).toBe(0);
  });

  it("shows the server's sentence when it refuses", async () => {
    const double = world();
    open();
    await screen.findByLabelText("New origin");
    double.failNext = { status: 422, body: { refusal: "invalid", detail: "port belongs to the daemon" } };
    type("http://localhost:5173");
    add();
    expect(await screen.findByText("port belongs to the daemon")).toBeTruthy();
  });
});

describe("parseLocalOrigin", () => {
  it("accepts loopback shapes only", () => {
    expect(parseLocalOrigin(" http://[::1]:5173 ")).toBe("http://[::1]:5173");
    expect(parseLocalOrigin("https://localhost:443")).toBe("https://localhost:443");
    expect(parseLocalOrigin("http://localhost:99999")).toBeNull();
    expect(parseLocalOrigin("http://localhost:5173/")).toBeNull();
  });
});
