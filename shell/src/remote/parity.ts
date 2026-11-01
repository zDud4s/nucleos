/**
 * The laptop works exactly as the tower: this table says, for every place the shell branches on
 * `isRemote()` and for every page in the nav, whether the remote shell behaves the same, behaves
 * the same through an adapter, or is an exception with a reason. `parity.test.ts` fails on a file
 * that branches on `isRemote()` or a nav page that has no row, so a new host-only branch cannot
 * land unclassified.
 *
 * `where` is a path under `shell/src` for a file, or `page:<nav id>` for a nav page.
 */
export type ParityStatus = "same" | "adapted" | "exception";

export interface ParityEntry {
  where: string;
  status: ParityStatus;
  why: string;
}

export const PARITY: ParityEntry[] = [
  // Files that branch on isRemote().
  { where: "app/BrowserBridge.tsx", status: "exception", why: "phase 5" },
  { where: "app/Dictation.tsx", status: "exception", why: "phase 5" },
  { where: "lib/vscode.ts", status: "exception", why: "phase 5" },
  { where: "data/files.ts", status: "exception", why: "phase 5" },
  { where: "pages/WebTabs.tsx", status: "exception", why: "phase 5" },
  { where: "ui/QueryState.tsx", status: "adapted", why: "a 403 on a remote device reads as unavailable remotely, not as an error" },
  { where: "ui/RefusalNote.tsx", status: "adapted", why: "a 403 on a remote device reads as unavailable remotely, not as an error" },

  // Nav pages.
  { where: "page:home", status: "same", why: "reads allowed and admin segments" },
  { where: "page:fleet", status: "same", why: "reads an allowed segment" },
  { where: "page:projects", status: "same", why: "reads an allowed segment" },
  { where: "page:autopilot", status: "same", why: "reads an allowed segment" },
  { where: "page:waiting", status: "same", why: "an admin device reaches it" },
  { where: "page:runs", status: "same", why: "reads an allowed segment" },
  { where: "page:analytics", status: "same", why: "reads an allowed segment" },
  { where: "page:feed", status: "same", why: "reads an allowed segment" },
  { where: "page:brain", status: "same", why: "an admin device reaches it" },
  { where: "page:chats", status: "same", why: "the chat API is `assistant`, an admin segment" },
  { where: "page:teams", status: "same", why: "an admin device reaches it" },
  { where: "page:agents", status: "same", why: "an admin device reaches it" },
  { where: "page:council", status: "same", why: "an admin device reaches it" },
  { where: "page:mail", status: "same", why: "an admin device reaches it" },
  { where: "page:contacts", status: "same", why: "an admin device reaches it" },
  { where: "page:calendar", status: "same", why: "an admin device reaches it" },
  { where: "page:voice", status: "exception", why: "phase 5" },
  { where: "page:web", status: "exception", why: "phase 5" },
  { where: "page:files", status: "adapted", why: "listing works; opening a file on disk stays on the host (phase 5)" },
  { where: "page:system", status: "exception", why: "phase 5" },
];
