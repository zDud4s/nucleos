import { useRef, useState } from "react";
import { FolderPlus, Globe, Plus, Upload } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import { useChats, useIdeSessions, usePatchChat } from "../data/chats";
import { isPicture } from "../lib/picture";

/** The largest text file that is inlined into the message. Beyond this it is a document, not context. */
export const MAX_TEXT_BYTES = 200 * 1024;

/** The tools "Browse the web" stands for. Both come and go together. */
const WEB_TOOLS = ["WebSearch", "WebFetch"];

const NO_CHAT = "open a conversation first";

/**
 * A text file as a fenced block the model reads as context.
 *
 * The fence is one backtick longer than the longest run inside the file, so a markdown file that
 * itself contains a fence cannot close the block early.
 */
export function fileAsContext(name: string, text: string): string {
  const longest = Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length));
  const fence = "`".repeat(Math.max(3, longest + 1));
  return `${fence}${name}\n${text}\n${fence}\n`;
}

/** Whether a file is text worth inlining: small, and not something the browser calls binary. */
function isInlineText(file: File): boolean {
  if (file.size > MAX_TEXT_BYTES) return false;
  if (file.type.startsWith("text/")) return true;
  if (file.type === "" || file.type === "application/json") return !/\.(exe|bin|zip|pdf)$/i.test(file.name);
  return /\+(json|xml)$/.test(file.type);
}

function readText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`could not read ${file.name}`));
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.readAsText(file);
  });
}

/**
 * The composer's "+": what can be added to a message besides words.
 *
 * Upload sends pictures to the picture path and inlines small text files; Add context widens what
 * the conversation's tools may reach; Browse the web switches the two web tools. The last two are
 * properties of a conversation, so with no `chatId` (the front door) they are shown disabled with
 * the reason rather than hidden.
 */
export function PlusMenu({
  chatId,
  onPictures,
  onText,
  onMention,
  disabled = false,
}: {
  chatId: string | null;
  onPictures: (files: File[]) => void;
  onText: (text: string) => void;
  /** The composer inserts an "@" at the caret, which opens its own file completion. */
  onMention: () => void;
  disabled?: boolean;
}) {
  const input = useRef<HTMLInputElement | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const chats = useChats();
  const row = chatId === null ? undefined : (chats.data ?? []).find((c) => c.chat_id === chatId);
  const sessions = useIdeSessions(chatId !== null);
  const patch = usePatchChat();

  const granted = row?.extra_dirs ?? [];
  const known = Array.from(
    new Set([...(sessions.data ?? []).map((session) => session.cwd), ...granted]),
  ).filter((folder) => folder !== row?.cwd);

  const denied = row?.denied_tools ?? [];
  const webOn = !WEB_TOOLS.some((tool) => denied.includes(tool));

  const toggleFolder = (folder: string, on: boolean) => {
    if (chatId === null) return;
    const next = on ? [...granted, folder] : granted.filter((path) => path !== folder);
    patch.mutate({ chatId, extra_dirs: next });
  };

  const toggleWeb = (on: boolean) => {
    if (chatId === null) return;
    const rest = denied.filter((tool) => !WEB_TOOLS.includes(tool));
    patch.mutate({ chatId, denied_tools: on ? rest : [...rest, ...WEB_TOOLS] });
  };

  const upload = async (files: FileList | null) => {
    const all = Array.from(files ?? []);
    const pictures = all.filter(isPicture);
    const rest = all.filter((file) => !isPicture(file));
    if (pictures.length > 0) onPictures(pictures);
    let refused = false;
    for (const file of rest) {
      if (!isInlineText(file)) {
        refused = true;
        continue;
      }
      try {
        const text = await readText(file);
        // A NUL byte is the plain tell of a binary file that wore a text type.
        if (text.includes("\u0000")) refused = true;
        else onText(fileAsContext(file.name, text));
      } catch {
        refused = true;
      }
    }
    setNote(refused ? "only pictures and text files can be attached" : null);
  };

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          className="chats-tool chats-plus"
          aria-label="Add to message"
          title="Add to message"
          disabled={disabled}
        >
          <Plus className="chats-tool-icon" aria-hidden="true" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="chats-meta-menu chats-plus-menu">
          <DropdownMenuItem onSelect={() => input.current?.click()}>
            <Upload className="chats-plus-icon" aria-hidden="true" />
            Upload from computer
          </DropdownMenuItem>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={chatId === null}>
              <FolderPlus className="chats-plus-icon" aria-hidden="true" />
              Add context
              {chatId === null && <span className="chats-tool-why">{NO_CHAT}</span>}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="chats-meta-menu">
              <DropdownMenuItem onSelect={onMention}>Mention a file…</DropdownMenuItem>
              <DropdownMenuLabel>Folders its tools may open</DropdownMenuLabel>
              {known.length === 0 && (
                <DropdownMenuItem disabled>no other folders known</DropdownMenuItem>
              )}
              {known.map((folder) => (
                <DropdownMenuCheckboxItem
                  key={folder}
                  checked={granted.includes(folder)}
                  disabled={patch.isPending}
                  onSelect={(event) => event.preventDefault()}
                  onCheckedChange={(on) => toggleFolder(folder, on === true)}
                >
                  {folder}
                </DropdownMenuCheckboxItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuCheckboxItem
            checked={chatId !== null && webOn}
            disabled={chatId === null || patch.isPending}
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(on) => toggleWeb(on === true)}
          >
            <Globe className="chats-plus-icon" aria-hidden="true" />
            Browse the web
            {chatId === null && <span className="chats-tool-why">{NO_CHAT}</span>}
          </DropdownMenuCheckboxItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <input
        ref={input}
        type="file"
        multiple
        className="sr-only"
        tabIndex={-1}
        aria-hidden="true"
        onChange={(event) => {
          void upload(event.target.files);
          // Cleared so the same file chosen twice in a row is heard the second time.
          event.target.value = "";
        }}
      />
      {note !== null && (
        <span className="chats-plus-note" role="status">
          {note}
        </span>
      )}
    </>
  );
}
