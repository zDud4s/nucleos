import { useState } from "react";
import { ApiRefusal } from "../data/client";
import {
  skipText,
  useChatMcp,
  useImportMcp,
  useSetChatMcp,
  type McpCardData,
  type McpImportReport,
  type McpServerRow,
} from "../data/mcp";
import {
  DropdownMenuCheckboxItem,
  DropdownMenuItem,
  DropdownMenuLabel,
} from "../ui/vendor/dropdown-menu";

/**
 * The MCP server registry as one conversation sees it, drawn two ways: as the body of the `/mcp`
 * slash card (plain checkboxes) and as items of the conversation's settings menu. Both switch a
 * server with the same mutation and import with the same call.
 */

/** The sentence a failed switch or import shows: the daemon's own when it refused. */
function errorText(error: unknown): string {
  if (error instanceof ApiRefusal) return error.detail || error.code;
  return error instanceof Error ? error.message : String(error);
}

/** What one server row says after its name: why it will not run, or a note about it. */
function rowWhy(server: McpServerRow): string | null {
  return server.inactive_reason ?? server.note ?? null;
}

/** The body of the `mcp` slash card. */
export function McpCardBody({ chatId, data }: { chatId: string; data: McpCardData }) {
  const set = useSetChatMcp(chatId);
  const importer = useImportMcp(chatId);
  // The card is an answer held in memory, not a live read: each toggle's reply replaces it.
  const [current, setCurrent] = useState<McpCardData>(data);
  const disabled = !current.supported;
  const toggle = (name: string, on: boolean) =>
    set.mutate(
      { name, on },
      {
        onSuccess: (next) => {
          if (next !== undefined) setCurrent(next);
        },
      },
    );

  return (
    <div className="chats-mcp-panel">
      <p>MCP servers ({current.engine}, mode {current.policy})</p>
      {current.engine_note != null && <p className="chats-tool-why">{current.engine_note}</p>}
      <ul>
        <li>
          <label>
            <input type="checkbox" checked={current.fixed.active} disabled readOnly />{" "}
            {current.fixed.name}
          </label>{" "}
          <span className="chats-tool-why">always on</span>
        </li>
        {current.servers.map((server) => {
          const why = rowWhy(server);
          return (
            <li key={server.name}>
              <label>
                <input
                  type="checkbox"
                  checked={server.enabled}
                  disabled={disabled}
                  onChange={(event) => toggle(server.name, event.target.checked)}
                />{" "}
                {server.name} <code>{server.command}</code>
                {server.env_names.length > 0 && ` env: ${server.env_names.join(", ")}`}
              </label>
              {why != null && (
                <>
                  {" "}
                  <span className="chats-tool-why">{why}</span>
                </>
              )}
            </li>
          );
        })}
      </ul>
      {current.servers.length === 0 && <p>No MCP servers in the registry</p>}
      {current.hint != null && <p>{current.hint}</p>}
      {set.isError && <p role="alert">{errorText(set.error)}</p>}
      <button type="button" disabled={importer.isPending} onClick={() => importer.mutate()}>
        Import
      </button>
      {importer.isError && <p role="alert">{errorText(importer.error)}</p>}
      {importer.data !== undefined && <McpImportBody data={importer.data} />}
    </div>
  );
}

/** The body of the `mcp_import` slash card: what an import did with every server it found. */
export function McpImportBody({ data }: { data: McpImportReport }) {
  const lines: { key: string; text: string }[] = [
    ...data.imported.map((name) => ({ key: `i:${name}`, text: `${name}: imported` })),
    ...data.skipped.map((s) => ({
      key: `s:${s.source}:${s.name}`,
      text: `${s.name}: ${skipText(s)}`,
    })),
    ...data.already_present.map((name) => ({ key: `p:${name}`, text: `${name}: already existed` })),
    ...data.unread.map((line, i) => ({ key: `u:${i}`, text: line })),
  ];
  if (lines.length === 0) return <p>No MCP servers found to import</p>;
  return (
    <ul className="chats-mcp-import">
      {lines.map((line) => (
        <li key={line.key}>{line.text}</li>
      ))}
    </ul>
  );
}

/** The registry's rows in the conversation's settings menu; nothing until the view has loaded. */
export function McpMenuItems({ chatId }: { chatId: string }) {
  const view = useChatMcp(chatId);
  const set = useSetChatMcp(chatId);
  const importer = useImportMcp(chatId);
  const data = view.data;
  if (data === undefined) return null;
  const disabled = !data.supported;

  return (
    <>
      <DropdownMenuLabel>MCP servers</DropdownMenuLabel>
      {data.engine_note != null && (
        <DropdownMenuLabel className="chats-tool-why">{data.engine_note}</DropdownMenuLabel>
      )}
      <DropdownMenuCheckboxItem
        className="chats-meta-toggle"
        checked={data.fixed.active}
        disabled
        onSelect={(event) => event.preventDefault()}
      >
        {data.fixed.name}
        <span className="chats-tool-why">always on</span>
      </DropdownMenuCheckboxItem>
      {data.servers.map((server) => {
        const why = rowWhy(server);
        return (
          <DropdownMenuCheckboxItem
            key={server.name}
            className="chats-meta-toggle"
            checked={server.enabled}
            disabled={disabled}
            title={server.command}
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(on) => set.mutate({ name: server.name, on: on === true })}
          >
            {server.name}
            {why != null && <span className="chats-tool-why">{why}</span>}
          </DropdownMenuCheckboxItem>
        );
      })}
      {data.hint != null && (
        <DropdownMenuLabel className="chats-tool-why">{data.hint}</DropdownMenuLabel>
      )}
      {set.isError && (
        <DropdownMenuLabel role="alert" className="chats-tool-why">
          {errorText(set.error)}
        </DropdownMenuLabel>
      )}
      <DropdownMenuItem
        disabled={importer.isPending}
        onSelect={(event) => {
          event.preventDefault();
          importer.mutate();
        }}
      >
        Import
      </DropdownMenuItem>
      {importer.isError && (
        <DropdownMenuLabel role="alert" className="chats-tool-why">
          {errorText(importer.error)}
        </DropdownMenuLabel>
      )}
    </>
  );
}
