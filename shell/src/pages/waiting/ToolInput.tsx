

/* ---------------------------------------------------------------- tool input -- */

export interface InputField {
  name: string;
  value: string;
}

/**
 * A proposal's arguments, as fields rather than as JSON.
 *
 * `null` out means the text was not an object — a bare command string, most
 * often — and the caller shows it as the one unnamed line it is. Braces, quotes
 * and indentation are dropped on purpose: the person reading this is deciding
 * whether an action may happen, and every character that is punctuation rather
 * than content is a character between them and that decision.
 */
export function readToolInput(raw: string): InputField[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return null;
  return Object.entries(parsed as Record<string, unknown>).map(
    ([name, value]) => ({
      name,
      value: plainValue(value),
    }),
  );
}

/** One value, flattened to a line of text. Nested structure is described, never dumped. */
export function plainValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === null) return "nothing";
  if (Array.isArray(value)) return value.map(plainValue).join(", ");
  if (typeof value === "object") {
    return Object.entries(value as Record<string, unknown>)
      .map(([name, nested]) => `${name} ${plainValue(nested)}`)
      .join("; ");
  }
  return String(value);
}

export function ToolInput({ raw }: { raw: string | null }) {
  if (raw === null || raw.trim() === "") return null;
  const fields = readToolInput(raw);
  // An object with no fields in it is an action that carried no arguments, and
  // rendering `{}` would be the one dump this whole function exists to avoid.
  if (fields !== null && fields.length === 0) return null;
  const lines: InputField[] = fields ?? [{ name: "input", value: raw.trim() }];
  return (
    <dl className="waiting-input">
      {lines.map((line) => (
        <div className="waiting-input-line" key={line.name}>
          <dt className="waiting-input-name">{line.name}</dt>
          <dd className="waiting-input-value">{line.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/* ---------------------------------------------------------------- the provenance -- */

/** One thing a turn read before it reached for an action. */
export interface ReadEntry {
  tool?: unknown;
  arguments?: unknown;
}

/**
 * The turn's reads, or `null` when there is nothing to show.
 *
 * Its own function because the card has to know *before* it renders whether the context column
 * has anything in it: a column of nothing would still take its 40% of the row.
 */
export function readEntries(raw: string | null): ReadEntry[] | null {
  if (raw === null || raw.trim() === "") return null;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed) || parsed.length === 0) return null;
    return parsed as ReadEntry[];
  } catch {
    return null;
  }
}

/**
 * Where the idea came from, under the record of what it was.
 *
 * `ToolInput` answers what was going to happen; this answers who put it in
 * the turn's head, and only the second one decides. "Email accounts and ask them
 * to change the bank details" reads identically whether the owner asked for it
 * or a page did, so a card carrying only the first question gets answered by
 * guessing.
 *
 * **Absent renders as nothing, never as "read nothing".** `null` covers two
 * cases the row cannot tell apart — a refusal that fired on whose work it is
 * rather than on what was read, and a recording that failed — and a card that
 * printed "this turn read nothing" would be wrong in the second one, in the
 * direction that makes a contaminated action look clean.
 */
export function ReadFrom({ raw }: { raw: string | null }) {
  const entries = readEntries(raw);
  if (entries === null) return null;
  return (
    <div className="waiting-provenance">
      <p className="waiting-provenance-head">this turn had read</p>
      <ul className="waiting-provenance-list">
        {entries.map((entry, index) => {
          const tool =
            typeof entry.tool === "string" ? entry.tool : "something unnamed";
          // The arguments are stored as the daemon received them, so the url that
          // decides the question is in there and nowhere else. Flattened through
          // the same reader the action's own input uses, for the same reason:
          // braces are characters between a person and a decision.
          const detail =
            typeof entry.arguments === "string"
              ? (readToolInput(entry.arguments) ?? [])
                  .map((field) => field.value)
                  .join(", ")
              : "";
          return (
            <li className="waiting-provenance-line" key={`${tool}-${index}`}>
              <span className="waiting-provenance-tool">{tool}</span>
              {detail !== "" && (
                <span className="waiting-provenance-detail">{detail}</span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
