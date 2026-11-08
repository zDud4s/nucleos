import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "../data/client";
import { accountKeys, useAccounts, windowOf, type AccountChoice, type PoolAccount } from "../data/accounts";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";

/** What `GET /assistant/chats/{id}/account` answers (`core/src/http/accounts/choice.rs`). */
export interface ChatAccount {
  chat_id: string;
  choice: AccountChoice;
  /** True while the chat has no row of its own and follows the default. */
  inherited: boolean;
  current_account_id: string | null;
  previous_account_id: string | null;
  switched_at: string | null;
}

const chatAccountKey = (chatId: string) => [...accountKeys.all, "chat", chatId] as const;
const chatAccountPath = (chatId: string) => `/assistant/chats/${encodeURIComponent(chatId)}/account`;

export function useChatAccount(chatId: string) {
  return useQuery({
    queryKey: chatAccountKey(chatId),
    queryFn: () => apiFetch<ChatAccount>(chatAccountPath(chatId)),
  });
}

export function useSetChatAccount(chatId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (choice: AccountChoice) =>
      apiFetch<{ chat_id: string; choice: AccountChoice }>(chatAccountPath(chatId), {
        method: "PUT",
        body: JSON.stringify({ choice }),
      }),
    retry: false,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: chatAccountKey(chatId) }),
  });
}

/** "2h 5m" until an ISO instant, or null when there is none or it has passed. */
export function resetsIn(iso: string | null | undefined, now: number = Date.now()): string | null {
  if (!iso) return null;
  const ms = Date.parse(iso) - now;
  if (!Number.isFinite(ms) || ms <= 0) return null;
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 60) return `${Math.max(1, minutes)}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d`;
}

function pct(fraction: number | null | undefined): string {
  return fraction === null || fraction === undefined ? "-" : `${Math.round(fraction * 100)}%`;
}

/** "5h 42% - 7d 10% - resets in 2h 5m". */
export function usageLine(account: PoolAccount, now: number = Date.now()): string {
  const five = windowOf(account, "5h");
  const seven = windowOf(account, "7d");
  const reset = resetsIn(five?.resets_at ?? seven?.resets_at, now);
  const parts = [`5h ${pct(five?.used_fraction)}`, `7d ${pct(seven?.used_fraction)}`];
  if (reset !== null) parts.push(`resets in ${reset}`);
  return parts.join(" · ");
}

/** The trigger's text: `Auto -> name` while following Auto, the account's name once pinned. */
export function chipText(choice: AccountChoice, currentLabel: string | null, accounts: PoolAccount[]): string {
  if (choice === "auto") return currentLabel === null ? "Auto" : `Auto → ${currentLabel}`;
  if (choice.startsWith("provider:")) return `Any ${choice.slice("provider:".length)}`;
  const id = choice.slice("account:".length);
  return accounts.find((a) => a.id === id)?.label ?? id;
}

function groupByProvider(accounts: PoolAccount[]): [string, PoolAccount[]][] {
  const map = new Map<string, PoolAccount[]>();
  for (const account of accounts) map.set(account.provider, [...(map.get(account.provider) ?? []), account]);
  return [...map.entries()];
}

/**
 * The chat's account chip, beside the model chip. Auto lets the pool choose; picking an account pins
 * the chat to it until it is set back to Auto (spec R1). Shows nothing while the pool is empty.
 */
export function AccountChip({ chatId }: { chatId: string }) {
  const accounts = useAccounts();
  const chat = useChatAccount(chatId);
  const set = useSetChatAccount(chatId);
  const list = accounts.data ?? [];
  if (list.length === 0 || chat.data === undefined) return null;

  const choice = chat.data.choice;
  const current = list.find((a) => a.id === chat.data.current_account_id);
  const text = chipText(choice, current?.label ?? null, list);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        className="chats-chip chats-chip-btn"
        aria-label={`Account: ${text}. Change the account`}
        title={choice === "auto" ? "The pool picks the account" : "Pinned until set back to Auto"}
      >
        {text}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="chats-meta-menu">
        <DropdownMenuRadioGroup value={choice} onValueChange={(next) => set.mutate(next)}>
          <DropdownMenuRadioItem value="auto">Auto</DropdownMenuRadioItem>
          {groupByProvider(list).map(([provider, members]) => (
            <div key={provider}>
              <DropdownMenuLabel>{provider}</DropdownMenuLabel>
              {members.map((account) => {
                const fraction = windowOf(account, "5h")?.used_fraction ?? null;
                return (
                  <DropdownMenuRadioItem key={account.id} value={`account:${account.id}`}>
                    <span>{account.label}</span>{" "}
                    <span
                      role="progressbar"
                      aria-label={`${account.label} 5h usage`}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={fraction === null ? undefined : Math.round(fraction * 100)}
                      style={{ display: "inline-block", width: 48, height: 4, background: "var(--line, #444)" }}
                    >
                      <span
                        style={{
                          display: "block",
                          height: "100%",
                          width: `${Math.min(100, Math.round((fraction ?? 0) * 100))}%`,
                          background: "currentColor",
                        }}
                      />
                    </span>{" "}
                    <small>{usageLine(account)}</small>
                  </DropdownMenuRadioItem>
                );
              })}
            </div>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
