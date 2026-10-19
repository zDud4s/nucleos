/**
 * Messaging channels: the types of the Control management routes
 * (`GET /channels`, pairing and identities). The shapes are fixed by annex E
 * of the channels design; they mirror the daemon's JSON field for field.
 */

export type ChannelState = "not_configured" | "starting" | "online" | "offline" | "incompatible";
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
