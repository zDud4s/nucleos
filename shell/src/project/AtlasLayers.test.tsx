// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { AtlasLayers } from "./AtlasLayers";
import { createAppQueryClient } from "../app/queryClient";
import type { ProjectMap } from "../data/project-map";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(undefined);
});

function mod(path: string) {
  return {
    path,
    reader: "rust",
    declares: false,
    cites: [],
    spec: null,
    tested: false,
  };
}

function map(): ProjectMap {
  return {
    modules: [mod("core/src/low/a.rs"), mod("core/src/mid/c.rs"), mod("core/src/top/d.rs")],
    imports: [
      { from: "core/src/top/d.rs", to: "core/src/mid/c.rs" },
      { from: "core/src/mid/c.rs", to: "core/src/low/a.rs" },
    ],
    unread: [],
    foreign: [],
    layers: {
      modules: {
        "core/src/low/a.rs": 0,
        "core/src/mid/c.rs": 1,
        "core/src/top/d.rs": 2,
      },
      overridden: [],
      violations: [],
    },
  } as unknown as ProjectMap;
}

function renderLayers() {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <AtlasLayers projectId="p1" data={map()} />
    </QueryClientProvider>,
  );
}

describe("AtlasLayers", () => {
  it("clicking a band isolates it and Escape returns", () => {
    renderLayers();
    expect(screen.getByRole("button", { name: /Layer 2/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Layer 1/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Layer 0/ })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /Layer 1/ }));
    expect(screen.getByRole("button", { name: /Layer 1/ }).getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(screen.queryByRole("button", { name: /Layer 2/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Layer 0/ })).toBeNull();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("button", { name: /Layer 2/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Layer 0/ })).toBeTruthy();
  });

  it("moving a module posts its new layer", async () => {
    renderLayers();
    fireEvent.click(screen.getByRole("button", { name: /top/ }));
    const select = screen.getByLabelText("Layer of core/src/top/d.rs");
    fireEvent.change(select, { target: { value: "0" } });

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/projects/p1/map/layers",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ path: "core/src/top/d.rs", layer: 0 }),
        }),
      ),
    );
  });

  it("a block in a lower band opens its own files when the folder repeats across layers", () => {
    const data = {
      modules: [mod("core/src/same/hi.rs"), mod("core/src/same/lo.rs")],
      imports: [{ from: "core/src/same/hi.rs", to: "core/src/same/lo.rs" }],
      unread: [],
      foreign: [],
      layers: {
        modules: { "core/src/same/hi.rs": 1, "core/src/same/lo.rs": 0 },
        overridden: [],
        violations: [],
      },
    } as unknown as ProjectMap;
    render(
      <QueryClientProvider client={createAppQueryClient()}>
        <AtlasLayers projectId="p1" data={data} />
      </QueryClientProvider>,
    );
    const blocks = screen.getAllByRole("button", { name: /same/ });
    expect(blocks).toHaveLength(2);
    fireEvent.click(blocks[1]);
    expect(screen.getByLabelText("Layer of core/src/same/lo.rs")).toBeTruthy();
    expect(screen.queryByLabelText("Layer of core/src/same/hi.rs")).toBeNull();
    const after = screen.getAllByRole("button", { name: /same/ });
    expect(after[0].getAttribute("aria-pressed")).toBe("false");
    expect(after[1].getAttribute("aria-pressed")).toBe("true");
  });
});
