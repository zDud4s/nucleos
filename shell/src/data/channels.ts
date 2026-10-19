/**
 * Messaging channels: the types of the Control management routes
 * (`GET /channels`, pairing and identities). The shapes are fixed by annex E
 * of the channels design; they mirror the daemon's JSON field for field.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

export type ChannelState ="not_configured" | "starting" | "online" | "offline" | "incompatible";
export type RoutingState = "ok" | "no_primary" | "primary_not_configured" | "primary_without_home";
export type ConversationKind = "private" | "group" | "thread";

export interface ChannelCapabilities {
  buttons: boolean;
  edit: boolean;
  typing: boolean;
  threads: boolean;
  reply_to: boolean;
  max_text: number;
  media_in: ("audio" | "image" | "file")[];
  media_out: ("audio" | "image" | "file")[];
}

export interface ChannelHome {
  conversation_ref: string;
  kind: ConversationKind;
  parent: string | null;
  sender_ref: string;
}

export interface Channel {
  id: string;
  kind: string;
  label: string;
  health_row: string;
  configured: boolean;
  missing_secrets: string[];
  settings_area: string;
  state: ChannelState;
  since: string | null;
  adapter: { contract_version: string } | null;
  capabilities: ChannelCapabilities | null;
  home: ChannelHome | null;
  identities: { senders: number; conversations: number };
  pairing: { expires_at: string; attempts_left: number } | null;
  outbox: { pending: number; leased: number; failed: number; oldest_due_at: string | null };
  drops: { unknown_sender: number; pairing_failures: number; counted_since: string };
}

export interface ChannelsReadout {
  contract_version: string;
  primary: string | null;
  routing: { state: RoutingState };
  channels: Channel[];
}

export interface PairingCode {
  code: string;
  command: string;
  expires_at: string;
  attempts_left: number;
}

export interface PairedIdentity {
  sender_ref: string;
  sender_name: string | null;
  conversation_ref: string;
  conversation_kind: ConversationKind;
  parent: string | null;
  home: boolean;
}

export type PairingStatus =
  | { state: "waiting"; expires_at: string; attempts_left: number }
  | { state: "paired"; paired_at: string; identity: PairedIdentity }
  | { state: "expired"; expires_at: string }
  | { state: "exhausted"; attempts_left: 0 }
  | { state: "superseded" }
  | { state: "none" };

export interface IdentityConversation {
  conversation_ref: string;
  kind: ConversationKind;
  parent: string | null;
  home: boolean;
  covers_threads: boolean;
  created_at: string;
}

export interface ChannelIdentity {
  sender_ref: string;
  sender_name: string | null;
  role: "owner";
  conversations: IdentityConversation[];
}

export interface ChannelIdentities {
  channel: string;
  home: string | null;
  identities: ChannelIdentity[];
  projects: { project_id: string; conversation_ref: string }[];
}

/* ------------------------------------------------------------------ hooks -- */

const channelPath = (channel: string) => `/channels/${encodeURIComponent(channel)}`;

/**
 * `GET /channels`. Thirty seconds, and only while an observer is mounted (the
 * channels page, the notifications scope selector): there is no permanent poll
 * for channels (E.9). A channel's state moves on the scale of minutes.
 */
export function useChannels() {
  return useQuery({
    queryKey: keys.channels.all,
    queryFn: () => apiFetch<ChannelsReadout>("/channels"),
    refetchInterval: POLL.slow,
  });
}

/**
 * `PUT /channels/primary` with `{channel}` (`null` clears it). Settling
 * refreshes the readout and the notification policy, whose per-channel view
 * carries the `primary` field.
 */
export function useSetPrimaryChannel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (channel: string | null) =>
      apiFetch<void>("/channels/primary", { method: "PUT", body: JSON.stringify({ channel }) }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.channels.all });
      void queryClient.invalidateQueries({ queryKey: keys.system.notifyPolicy });
    },
  });
}

/**
 * `POST /channels/{channel}/pairing`. The code leaves the daemon only in this
 * answer, so it is returned to the caller (the dialog's state) and never put in
 * the cache. What goes in the cache is the status `waiting`, which REPLACES any
 * earlier terminal state (`expired`, `exhausted`, `superseded`, `paired`) and so
 * restarts the dialog's poll.
 */
export function useStartPairing(channel: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<PairingCode>(`${channelPath(channel)}/pairing`, { method: "POST" }),
    retry: false,
    onSuccess: (pairing) => {
      const waiting: PairingStatus = {
        state: "waiting",
        expires_at: pairing.expires_at,
        attempts_left: pairing.attempts_left,
      };
      queryClient.setQueryData(keys.channels.pairing(channel), waiting);
    },
  });
}

/**
 * `GET /channels/{channel}/pairing`. The only fast poll of the channels
 * surface, and it runs only while the dialog is open (`enabled`) and the code is
 * still live: it stops itself on every terminal state.
 *
 * `none` is not a final state while the code the dialog just got is unexpired
 * (`expiresAt`): it is the race between the POST and the first GET. After the
 * code's own expiry, or with no `expiresAt`, `none` ends the poll.
 */
export function usePairingStatus(channel: string, options: { enabled: boolean; expiresAt?: string }) {
  const { enabled, expiresAt } = options;
  return useQuery({
    queryKey: keys.channels.pairing(channel),
    queryFn: () => apiFetch<PairingStatus>(`${channelPath(channel)}/pairing`),
    enabled,
    refetchInterval: (query: { state: { data: PairingStatus | undefined } }) => {
      const data = query.state.data;
      if (data === undefined || data.state === "waiting") return POLL.fast;
      if (data.state === "none") {
        const until = expiresAt === undefined ? Number.NaN : Date.parse(expiresAt);
        return Number.isFinite(until) && until > Date.now() ? POLL.fast : false;
      }
      return false;
    },
  });
}

/** `DELETE /channels/{channel}/pairing`: idempotent cancel of the active code. */
export function useCancelPairing(channel: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<void>(`${channelPath(channel)}/pairing`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.channels.pairing(channel) });
      void queryClient.invalidateQueries({ queryKey: keys.channels.all });
    },
  });
}

/**
 * `GET /channels/{channel}/identities`. No `refetchInterval`: the list changes
 * only by a mutation or a pairing, and both invalidate it.
 */
export function useChannelIdentities(channel: string) {
  return useQuery({
    queryKey: keys.channels.identities(channel),
    queryFn: () => apiFetch<ChannelIdentities>(`${channelPath(channel)}/identities`),
  });
}

function invalidateIdentities(queryClient: ReturnType<typeof useQueryClient>, channel: string) {
  void queryClient.invalidateQueries({ queryKey: keys.channels.identities(channel) });
  void queryClient.invalidateQueries({ queryKey: keys.channels.all });
}

/** `DELETE /channels/{channel}/identities/{sender}[?conversation=]`: one conversation, or the whole sender. */
export function useRemoveIdentity(channel: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ sender, conversation }: { sender: string; conversation?: string }) =>
      apiFetch<{ removed: number; home_cleared: boolean }>(
        `${channelPath(channel)}/identities/${encodeURIComponent(sender)}` +
          (conversation === undefined ? "" : `?conversation=${encodeURIComponent(conversation)}`),
        { method: "DELETE" },
      ),
    retry: false,
    onSettled: () => invalidateIdentities(queryClient, channel),
  });
}

/** `PUT /channels/{channel}/identities/{sender}/home`: make one of the sender's conversations the home. */
export function useSetHomeConversation(channel: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ sender, conversation_ref }: { sender: string; conversation_ref: string }) =>
      apiFetch<{ home: string; previous: string | null }>(
        `${channelPath(channel)}/identities/${encodeURIComponent(sender)}/home`,
        { method: "PUT", body: JSON.stringify({ conversation_ref }) },
      ),
    retry: false,
    onSettled: () => invalidateIdentities(queryClient, channel),
  });
}
