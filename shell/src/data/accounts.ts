import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { POLL, pollWhile } from "./poll";

/**
 * The LLM account pool, as hooks: the list, the default choice, the guided login and the
 * per-account writes. Shapes are read off `core/src/http/accounts.rs` and `accounts/*.rs`.
 *
 * Account ids contain `:` (`claude:work`), so every path segment goes through {@link accountPath}.
 */

/** Query keys, kept here because the account pool owns them. */
export const accountKeys = {
  all: ["accounts"] as const,
  list: ["accounts", "list"] as const,
  default: ["accounts", "default"] as const,
  login: (loginId: string) => ["accounts", "login", loginId] as const,
};

export type AccountState = "active" | "paused" | "needs_login";
export type AccountKind = "subscription" | "api_key";

/** One usage window of an account (`5h`, `7d`, ...). */
export interface UsageWindow {
  name: string;
  /** 0..1, or null when the provider did not say. */
  used_fraction: number | null;
  resets_at: string | null;
  stale: boolean;
}

export interface AccountUsage {
  /** The reading's fidelity: where the numbers came from. */
  source: string;
  read_at: string | null;
  windows: UsageWindow[];
}

export interface PoolAccount {
  id: string;
  engine: string;
  provider: string;
  label: string;
  kind: AccountKind;
  state: AccountState;
  is_default: boolean;
  created_at: string;
  updated_at: string;
  live_sessions: number;
  usage: AccountUsage | null;
}

/** `auto` | `provider:<provider>` | `account:<id>`. */
export type AccountChoice = string;

export type LoginPhase =
  | "starting"
  | "awaiting_browser"
  | "awaiting_code"
  | "verifying"
  | "done"
  | "failed"
  | "cancelled";

export interface LoginView {
  login_id: string;
  account_id: string;
  engine: string;
  label: string;
  phase: LoginPhase;
  auth_url: string | null;
  message: string | null;
}

export function isLoginFinished(phase: LoginPhase): boolean {
  return phase === "done" || phase === "failed" || phase === "cancelled";
}

/** `/accounts/<encoded id>` plus an optional suffix. */
export function accountPath(id: string, suffix = ""): string {
  return `/accounts/${encodeURIComponent(id)}${suffix}`;
}

/** A usage window by name, or undefined when the account has no such reading. */
export function windowOf(account: PoolAccount, name: string): UsageWindow | undefined {
  return account.usage?.windows.find((w) => w.name === name);
}

/** `GET /accounts`. `engines` is absent from a daemon older than the field. */
export interface AccountsResponse {
  accounts: PoolAccount[];
  engines?: string[];
}

/** The list query, shared by {@link useAccounts} and {@link usePooledEngines}: one fetch, one cache entry. */
function accountsQuery() {
  return {
    queryKey: accountKeys.list,
    queryFn: () => apiFetch<AccountsResponse>("/accounts"),
    refetchInterval: POLL.queue,
  };
}

export function useAccounts() {
  return useQuery({ ...accountsQuery(), select: (answer: AccountsResponse) => answer.accounts });
}

/** The engines a new account can be added for; undefined until loaded or from an older daemon. */
export function usePooledEngines() {
  return useQuery({ ...accountsQuery(), select: (answer: AccountsResponse) => answer.engines });
}

export function useAccountDefault() {
  return useQuery({
    queryKey: accountKeys.default,
    queryFn: async () => (await apiFetch<{ choice: AccountChoice }>("/account-pool/default")).choice,
  });
}

export function useSetAccountDefault() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (choice: AccountChoice) =>
      apiFetch<{ choice: AccountChoice }>("/account-pool/default", {
        method: "PUT",
        body: JSON.stringify({ choice }),
      }),
    retry: false,
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: accountKeys.default }),
  });
}

/** Starts the guided login of a new account; answers 202 with the login view. */
export function useStartAccount() {
  return useMutation({
    mutationFn: (body: { engine: string; label: string }) =>
      apiFetch<LoginView>("/accounts", { method: "POST", body: JSON.stringify(body) }),
    retry: false,
  });
}

/** Starts the login of an existing account again. */
export function useReloginAccount() {
  return useMutation({
    mutationFn: (id: string) => apiFetch<LoginView>(accountPath(id, "/login"), { method: "POST" }),
    retry: false,
  });
}

/** A login in progress, polled until it reaches a terminal phase. */
export function useLogin(loginId: string | null) {
  const queryClient = useQueryClient();
  return useQuery({
    queryKey: accountKeys.login(loginId ?? ""),
    enabled: loginId !== null,
    queryFn: async () => {
      const view = await apiFetch<LoginView>(`/account-logins/${encodeURIComponent(loginId ?? "")}`);
      if (isLoginFinished(view.phase)) void queryClient.invalidateQueries({ queryKey: accountKeys.list });
      return view;
    },
    refetchInterval: pollWhile<LoginView>(POLL.turn, (view) => !isLoginFinished(view.phase)),
  });
}

export function useSubmitLoginCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ loginId, code }: { loginId: string; code: string }) =>
      apiFetch<LoginView>(`/account-logins/${encodeURIComponent(loginId)}/code`, {
        method: "POST",
        body: JSON.stringify({ code }),
      }),
    retry: false,
    onSuccess: (view) => queryClient.setQueryData(accountKeys.login(view.login_id), view),
  });
}

export function useCancelLogin() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (loginId: string) =>
      apiFetch<LoginView>(`/account-logins/${encodeURIComponent(loginId)}`, { method: "DELETE" }),
    retry: false,
    onSuccess: (view) => {
      queryClient.setQueryData(accountKeys.login(view.login_id), view);
      void queryClient.invalidateQueries({ queryKey: accountKeys.list });
    },
  });
}

/** Rename, pause, resume and remove: each refreshes the list. */
export function useAccountActions() {
  const queryClient = useQueryClient();
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: accountKeys.all });
  };
  const rename = useMutation({
    mutationFn: ({ id, label }: { id: string; label: string }) =>
      apiFetch<PoolAccount>(accountPath(id, "/rename"), { method: "POST", body: JSON.stringify({ label }) }),
    retry: false,
    onSuccess: refresh,
  });
  const pause = useMutation({
    mutationFn: (id: string) => apiFetch<PoolAccount>(accountPath(id, "/pause"), { method: "POST" }),
    retry: false,
    onSuccess: refresh,
  });
  const resume = useMutation({
    mutationFn: (id: string) => apiFetch<PoolAccount>(accountPath(id, "/resume"), { method: "POST" }),
    retry: false,
    onSuccess: refresh,
  });
  const remove = useMutation({
    mutationFn: ({ id, force }: { id: string; force?: boolean }) =>
      apiFetch<{ removed: string }>(`${accountPath(id)}${force ? "?force=true" : ""}`, { method: "DELETE" }),
    retry: false,
    onSuccess: refresh,
  });
  return { rename, pause, resume, remove };
}
