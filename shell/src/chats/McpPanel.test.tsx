import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { McpCardBody, McpImportBody, McpMenuItems } from "./McpPanel";
import { createAppQueryClient } from "../app/queryClient";
import type { McpCardData, McpImportReport } from "../data/mcp";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";

function card(overrides: Partial<McpCardData> = {}): McpCardData {
  return {
    engine: "claude",
    supported: true,
    engine_note: null,
    policy: "unrestricted",
    fixed: { name: "nucleos", active: true },
    servers: [
      {
        name: "gh",
        command: "gh-mcp",
        env_names: [],
        imported_from: "claude-user",
        enabled: true,
        active: true,
        inactive_reason: null,
        note: null,
      },
      {
        name: "fs",
        command: "fs-mcp",
        env_names: ["TOKEN"],
        imported_from: null,
        enabled: false,
        active: false,
        inactive_reason: null,
        note: null,
      },
    ],
    hint: null,
    ...overrides,
  };
}

const report: McpImportReport = {
  imported: ["alpha"],
  skipped: [
    { name: "web", source: "claude-user", reason: "http_transport" },
    { name: "gh", source: "codex", reason: "duplicate", kept_from: "claude-user" },
  ],
  already_present: ["beta"],
  unread: ["config.toml: expected a table"],
};

function wrap(node: ReactNode) {
  return render(<QueryClientProvider client={createAppQueryClient()}>{node}</QueryClientProvider>);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/assistant/mcp/import") return report;
    return card();
  });
});

describe("McpCardBody", () => {
  it("shows one checkbox per server with its state, and nucleos fixed", () => {
    wrap(<McpCardBody chatId="c-1" data={card()} />);
    expect((screen.getByRole("checkbox", { name: /gh/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /fs/ }) as HTMLInputElement).checked).toBe(false);
    const fixed = screen.getByRole("checkbox", { name: /nucleos/ }) as HTMLInputElement;
    expect(fixed.checked).toBe(true);
    expect(fixed.disabled).toBe(true);
    expect(screen.getByText(/TOKEN/)).toBeTruthy();
  });

  it("ticking a server sends PUT and unticking sends DELETE", async () => {
    wrap(<McpCardBody chatId="c-1" data={card()} />);
    fireEvent.click(screen.getByRole("checkbox", { name: /fs/ }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/mcp/fs", {
        method: "PUT",
      }),
    );
    fireEvent.click(screen.getByRole("checkbox", { name: /gh/ }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/mcp/gh", {
        method: "DELETE",
      }),
    );
  });

  it("shows an inactive reason, a note and the hint", () => {
    const data = card({ hint: "this chat used Claude's servers; use Import" });
    data.servers[0] = {
      ...data.servers[0],
      active: false,
      inactive_reason: "mode mcponly only allows NucleOS tools",
    };
    data.servers[1] = { ...data.servers[1], note: "Codex keeps its own servers" };
    wrap(<McpCardBody chatId="c-1" data={data} />);
    expect(screen.getByText("mode mcponly only allows NucleOS tools")).toBeTruthy();
    expect(screen.getByText("Codex keeps its own servers")).toBeTruthy();
    expect(screen.getByText("this chat used Claude's servers; use Import")).toBeTruthy();
  });

  it("an unsupported engine disables every checkbox and says why", () => {
    wrap(
      <McpCardBody
        chatId="c-1"
        data={card({
          supported: false,
          engine: "local",
          engine_note: "not supported on this engine in v1",
          fixed: { name: "nucleos", active: false },
        })}
      />,
    );
    for (const box of screen.getAllByRole("checkbox")) {
      expect((box as HTMLInputElement).disabled).toBe(true);
    }
    expect(screen.getByText("not supported on this engine in v1")).toBeTruthy();
  });

  it("Import posts the chat id and shows the report", async () => {
    wrap(<McpCardBody chatId="c-1" data={card()} />);
    fireEvent.click(screen.getByRole("button", { name: "Import" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/mcp/import", {
        method: "POST",
        body: JSON.stringify({ chat_id: "c-1" }),
      }),
    );
    expect(await screen.findByText(/alpha: imported/)).toBeTruthy();
  });
});

describe("McpImportBody", () => {
  it("renders imported, skipped with reason, already present and unread", () => {
    wrap(<McpImportBody data={report} />);
    expect(screen.getByText("alpha: imported")).toBeTruthy();
    expect(screen.getByText("web: not imported: http transport")).toBeTruthy();
    expect(screen.getByText("gh: ignored: repeated name (kept from claude-user)")).toBeTruthy();
    expect(screen.getByText("beta: already existed")).toBeTruthy();
    expect(screen.getByText("config.toml: expected a table")).toBeTruthy();
  });

  it("says so when nothing was found", () => {
    wrap(<McpImportBody data={{ imported: [], skipped: [], already_present: [], unread: [] }} />);
    expect(screen.getByText("No MCP servers found to import")).toBeTruthy();
  });
});

describe("McpMenuItems", () => {
  it("lists the servers as menu checkboxes and toggles one", async () => {
    wrap(
      <DropdownMenu open>
        <DropdownMenuTrigger>settings</DropdownMenuTrigger>
        <DropdownMenuContent>
          <McpMenuItems chatId="c-1" />
        </DropdownMenuContent>
      </DropdownMenu>,
    );
    expect(await screen.findByText("MCP servers")).toBeTruthy();
    const gh = await screen.findByRole("menuitemcheckbox", { name: /gh/ });
    expect(gh.getAttribute("aria-checked")).toBe("true");
    const fs = screen.getByRole("menuitemcheckbox", { name: /fs/ });
    expect(fs.getAttribute("aria-checked")).toBe("false");
    expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/mcp");
    fireEvent.click(fs);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/mcp/fs", {
        method: "PUT",
      }),
    );
  });
});
