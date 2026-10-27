import type { Command, CommandCard, CommandSource } from "../data/chats";

/** Where a command came from, in the words the picker and the help card both use. */
export function sourceLabel(source: CommandSource, kind?: Command["kind"]): string {
  const where =
    source === "builtin"
      ? "built-in"
      : source === "project"
        ? "projeto"
        : source === "personal"
          ? "pessoal"
          : "plugin";
  return kind === "skill" ? `skill · ${where}` : where;
}

interface HelpEntry {
  name: string;
  kind?: "command" | "skill";
  hint?: string | null;
  description?: string | null;
  aliases?: string[];
  partial?: string[];
  shadowed_by?: string | null;
}

interface HelpGroup {
  source: CommandSource;
  entries: HelpEntry[];
}

function EntryLine({ entry, source }: { entry: HelpEntry; source: CommandSource }) {
  return (
    <li className="chats-slash-entry">
      <span className="chats-mention-name">
        /{entry.name}
        {entry.hint ? ` ${entry.hint}` : ""}
      </span>{" "}
      <span className="chats-mention-path">
        {entry.description ?? ""}
        {entry.aliases && entry.aliases.length > 0
          ? ` (also ${entry.aliases.map((a) => `/${a}`).join(", ")})`
          : ""}
        {entry.shadowed_by ? ` · shadowed by ${entry.shadowed_by}` : ""}
      </span>{" "}
      <span className="chats-slash-source">{sourceLabel(source, entry.kind)}</span>
    </li>
  );
}

/**
 * What a slash command answered with, drawn beside the transcript. Held in memory only: a card is
 * an answer to the person at the keyboard, not part of the conversation, so it is never replayed.
 */
export function SlashCard({ card, onDismiss }: { card: CommandCard; onDismiss: () => void }) {
  const data = (card.data ?? {}) as {
    groups?: HelpGroup[];
    entry?: HelpEntry;
    source?: CommandSource;
  };
  return (
    <section className="chats-slash-card" aria-label={card.kind === "help" ? "Commands" : "Command"}>
      {card.kind === "help" &&
        (data.groups ?? []).map((group) => (
          <div key={group.source}>
            <h4 className="chats-slash-group">{sourceLabel(group.source)}</h4>
            <ul>
              {(group.entries ?? []).map((entry) => (
                <EntryLine key={entry.name} entry={entry} source={group.source} />
              ))}
            </ul>
          </div>
        ))}
      {card.kind === "help_entry" && data.entry !== undefined && (
        <ul>
          <EntryLine entry={data.entry} source={data.source ?? "builtin"} />
        </ul>
      )}
      <button type="button" className="chats-slash-dismiss" onClick={onDismiss}>
        Dismiss
      </button>
    </section>
  );
}
