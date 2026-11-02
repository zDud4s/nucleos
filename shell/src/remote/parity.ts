/**
 * The laptop works exactly as the tower: this table says, for every place the shell branches on
 * `isRemote` / `isHostOnlyError` (imported from `remote/host`) and for every page in the nav,
 * whether the remote shell behaves the same, behaves the same through an adapter, or is an
 * exception with a reason. `parity.test.ts` fails on a file that imports them or a nav page that
 * has no row, and on a file row whose file no longer does, so a new host-only branch cannot land
 * unclassified.
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
  { where: "app/BrowserBridge.tsx", status: "exception", why: "owner tabs use host native webviews and are not started remotely; replacing them with a tower Chrome screencast is the owner's decision, with measurements coming from the task 5.5 script" },
  { where: "app/Dictation.tsx", status: "adapted", why: "remotely the browser captures with lib/capture.ts, posts to /voice/capture and types the text into the field focused when recording started, or copies it to the clipboard (shown if the copy is refused); no host command is called (task 5.2)" },
  { where: "data/hotkeys.ts", status: "exception", why: "an OS-wide chord cannot be registered from a browser tab; remotely the registration is skipped and the Voice page says the shortcuts only work while the page is focused, where PageDictationShortcuts answers them (task 5.2)" },
  { where: "data/machines.ts", status: "exception", why: "pairings live in the installed shell's storage and OS vault, and requests to a paired machine are made by the host; a browser tab lists only its own machine and never learns the desktop's pairings (task 6.3)" },
  { where: "lib/vscode.ts", status: "exception", why: "VS Code opens on the host; remotely openInVscode rejects with a sentence (the editor opens on the main machine only); the three call sites with a catch show it, and the five without one hide their button (task 5.3)" },
  { where: "data/files.ts", status: "adapted", why: "remotely \"open on disk\" downloads the file as a native browser download (an anchor to /files/download, so a large file streams to disk; a failed one shows in the browser's downloads list, not in the page), because the disk is the host's; the Files page takes drops through the DOM and the File API and uploads them by the picker's route (a dropped folder is refused in words); ContextRefs cannot take a drop, since a context ref is a host path a browser never has, and says to type it (task 5.3)" },
  { where: "pages/NewProject.tsx", status: "adapted", why: "remotely Browse opens PathPicker, which lists directories from the tower only for an owner device; desktop Browse keeps its native dialog (task 5.4)" },
  { where: "app/desktop-toast.ts", status: "adapted", why: "remotely a toast is a Web Notification, shown only when the browser already granted permission; permission is requested from a click in System, never on load (task 5.3)" },
  { where: "pages/Browser.tsx", status: "adapted", why: "remotely the live-session list remains visible even from a Windows laptop, because no native WebView replaces it; its owner-authorised live stream uses the gateway (task 5.5)" },
  { where: "pages/WebTabs.tsx", status: "adapted", why: "remotely /web shows the API-backed agent-session panel and live pixels, while the Archive and /web/pages/$id render directly; native owner tabs remain unavailable (task 5.5)" },
  { where: "ui/QueryState.tsx", status: "adapted", why: "a 403 on a remote device reads as unavailable remotely, not as an error" },
  { where: "ui/RefusalNote.tsx", status: "adapted", why: "a 403 on a remote device reads as unavailable remotely, not as an error" },

  // Nav pages.
  { where: "page:home", status: "same", why: "reads allowed and admin segments; imports no LiveView, AgentPanel or AgentControlBar, so no browser pixels" },
  { where: "page:fleet", status: "same", why: "reads an allowed segment" },
  { where: "page:projects", status: "same", why: "reads an allowed segment" },
  { where: "page:autopilot", status: "same", why: "reads an allowed segment" },
  { where: "page:waiting", status: "same", why: "an admin device reaches it; a wheel approval only hands over a seat, the live pixels it opens stay host-only on page:web (phase 5)" },
  { where: "page:runs", status: "same", why: "reads an allowed segment; the run list is this machine's own, a run started on another machine is not listed or followed here" },
  { where: "page:analytics", status: "same", why: "reads an allowed segment" },
  { where: "page:feed", status: "same", why: "reads an allowed segment" },
  { where: "page:brain", status: "same", why: "its settings data calls /config/*, which only an owner device reaches" },
  { where: "page:chats", status: "same", why: "the chat API is `assistant`, an admin segment; imports no LiveView, AgentPanel or AgentControlBar, which only page:web pulls" },
  { where: "page:teams", status: "same", why: "an admin device reaches it" },
  { where: "page:agents", status: "same", why: "an admin device reaches it" },
  { where: "page:council", status: "same", why: "an admin device reaches it" },
  { where: "page:mail", status: "same", why: "its account data calls /config/email, which only an owner device reaches" },
  { where: "page:contacts", status: "same", why: "an admin device reaches it" },
  { where: "page:calendar", status: "same", why: "an admin device reaches it" },
  { where: "page:voice", status: "adapted", why: "voice is an admin segment, so an admin or owner device reaches it; the capture buttons dictate through the browser microphone, and the global hotkeys become in-page shortcuts (see data/hotkeys.ts); hands-free conversation is not covered here (task 5.2)" },
  { where: "page:web", status: "adapted", why: "agent sessions and their live pixels use the owner /browser API, and Archive routes use the admin /web API; native owner tabs remain an exception because only the owner can decide whether to replace them with a tower Chrome screencast" },
  { where: "page:files", status: "adapted", why: "listing, upload, download and browser drag-and-drop work; \"open on disk\" downloads the file instead (task 5.3)" },
  { where: "page:system", status: "adapted", why: "every family it calls (system, config, sidecars, backups, api-tokens, remote, channels) is an owner segment; the host's own update cannot be installed from a browser, so no update notice appears (task 5.8); read statically, the real walk is pending" },
];
