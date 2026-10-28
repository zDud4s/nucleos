import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The MCP server registry, as hooks: one conversation's view of it (which servers it has switched
 * on, and which of those its tool policy lets run), the switches, and the import from Claude's and
 * Codex's own configs. Env values never reach the shell — the daemon sends only their names.
 */

/** Where an imported server came from; `null` for one added by hand. */
export type McpSource = "claude-user" | "claude-project" | "codex";

/** One registry server, as `GET /assistant/mcp/servers` lists it. */
export interface McpServerView {
  name: string;
  command: string;
  env_names: string[];
  imported_from: McpSource | null;
}

/** One registry server as a conversation sees it: a row of `McpCardData.servers`. */
export interface McpServerRow extends McpServerView {
  /** Switched on for this conversation. */
  enabled: boolean;
  /** Switched on and allowed by the turn's tool policy: the next turn gets it. */
  active: boolean;
  /** Why an enabled server will not run, when it will not; `null` otherwise. */
  inactive_reason: string | null;
  note: string | null;
}

/**
 * `GET /assistant/chats/{id}/mcp`, the answer of a toggle, and the `data` of the `mcp` slash card.
 */
export interface McpCardData {
  engine: string;
  /** Whether this conversation's engine takes registry servers at all. */
  supported: boolean;
  engine_note: string | null;
  policy: string;
  /** NucleOS's own server: always on wherever servers are supported, never switchable. */
  fixed: { name: string; active: boolean };
  servers: McpServerRow[];
  hint: string | null;
}

/** A server an import passed over, and why. */
export interface McpSkipped {
  name: string;
  source: McpSource | string;
  reason: "http_transport" | "duplicate" | "invalid_name" | "missing_command" | "malformed" | string;
  /** For `duplicate`: the source whose server of that name was kept. */
  kept_from?: string;
}

/** `POST /assistant/mcp/import`, and the `data` of the `mcp_import` slash card. */
export interface McpImportReport {
  imported: string[];
  skipped: McpSkipped[];
  already_present: string[];
  /** `"<file>: <error>"` for a config that exists but could not be read. */
  unread: string[];
}

function chatMcpPath(chatId: string): string {
  return `/assistant/chats/${encodeURIComponent(chatId)}/mcp`;
}

/** This conversation's view of the registry. */
export function useChatMcp(chatId: string) {
  return useQuery({
    queryKey: keys.chats.mcp(chatId),
    queryFn: () => apiFetch<McpCardData>(chatMcpPath(chatId)),
  });
}

/** The whole registry, apart from any conversation. */
export function useMcpServers() {
  return useQuery({
    queryKey: keys.mcp.servers,
    queryFn: () => apiFetch<{ servers: McpServerView[] }>("/assistant/mcp/servers"),
  });
}

/**
 * Switch one server on (`PUT`) or off (`DELETE`) for this conversation. The daemon answers the
 * updated card, which becomes the cached view at once; a refusal is an `ApiRefusal` whose
 * `detail` is the sentence to show.
 */
export function useSetChatMcp(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ name, on }: { name: string; on: boolean }) =>
      apiFetch<McpCardData>(`${chatMcpPath(chatId)}/${encodeURIComponent(name)}`, {
        method: on ? "PUT" : "DELETE",
      }),
    retry: false,
    onSuccess: (data) => {
      if (data !== undefined) queryClient.setQueryData(keys.chats.mcp(chatId), data);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.mcp(chatId) });
    },
  });
}

/**
 * Import servers from Claude's and Codex's configs into the registry. With a conversation, its
 * project's own config is read too. Every conversation's view changes, so all of them refresh.
 */
export function useImportMcp(chatId?: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      apiFetch<McpImportReport>("/assistant/mcp/import", {
        method: "POST",
        body: JSON.stringify(chatId === undefined ? {} : { chat_id: chatId }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.mcpAll });
      void queryClient.invalidateQueries({ queryKey: keys.mcp.servers });
    },
  });
}

/** Remove a server from the registry, for every conversation. */
export function useDeleteMcpServer() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      apiFetch<void>(`/assistant/mcp/servers/${encodeURIComponent(name)}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.chats.mcpAll });
      void queryClient.invalidateQueries({ queryKey: keys.mcp.servers });
    },
  });
}

/** The sentence for one skipped server, after its name. */
export function skipText(skipped: McpSkipped): string {
  switch (skipped.reason) {
    case "http_transport":
      return "not imported: http transport";
    case "duplicate":
      return `ignored: repeated name (kept from ${skipped.kept_from ?? "an earlier source"})`;
    case "invalid_name":
      return "not imported: invalid name";
    case "missing_command":
      return "not imported: no command";
    case "malformed":
      return "not imported: malformed entry";
    default:
      return `not imported: ${skipped.reason.replace(/_/g, " ")}`;
  }
}
