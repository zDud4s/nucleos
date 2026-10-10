import type { ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { CopyButton } from "../ui";
import {
  blocks,
  lines,
  spans as spansOf,
  type Block as RichBlock,
  type Line as RichLine,
  type Span as RichSpan,
} from "../lib/rich";

/**
 * A model's answer, drawn as the shapes it was written in.
 *
 * The parser is in `lib/rich.ts` and returns data, never markup; every element below is chosen
 * here, from a closed set. So a transcript containing a script tag is a string containing a script
 * tag at every step of this, and there is no path by which one talks this into rendering HTML.
 *
 * Only the model's half goes through it. What a person typed is drawn exactly as they typed it.
 */
export function Rich({ text }: { text: string }) {
  return (
    <>
      {blocks(text).map((block, index) =>
        block.kind === "code" ? (
          /* The block, and the one gesture anybody performs on one. A wrapper rather than a
             button inside the `<pre>`: the `<pre>` scrolls sideways, and a control placed in a
             scrolling box slides out of its own corner the moment the code is wider than the
             column — which is exactly when somebody wants to copy it rather than read it. */
          <div key={index} className="chats-code-block">
            <pre className="chats-code">
              <code>{block.text}</code>
            </pre>
            <span className="chats-code-copy">
              <CopyButton value={block.text} label="this code" spoken={false} />
            </span>
          </div>
        ) : block.kind === "table" ? (
          <RichTable key={index} table={block} />
        ) : (
          <div key={index} className="chats-prose">
            {lines(block.text).map((line, at) => (
              <RichLineOut key={at} line={line} />
            ))}
          </div>
        ),
      )}
    </>
  );
}

/**
 * A table, as a table.
 *
 * It used to be five rows of pipes: `blocks` had no idea one existed, so the whole thing went
 * through the prose path and came out as the characters it was made of. A comparison of three
 * years against three rules is the single most useful shape an agent writes, and it was the one
 * this drew worst.
 *
 * Scrolls inside its own frame rather than widening the column. A six-column table in a
 * conversation is ordinary, and the alternative to scrolling is either a transcript that scrolls
 * sideways as a whole or cells folded until the table stops being one.
 */
function RichTable({
  table,
}: {
  table: Extract<RichBlock, { kind: "table" }>;
}) {
  // The author's own alignment, mapped to a class rather than an inline style: this window runs
  // under a CSP with no `unsafe-inline`, so a `style` attribute is not a thing it can write.
  const align = (at: number) => {
    const set = table.align[at];
    return set === null || set === undefined
      ? "chats-table-cell"
      : `chats-table-cell chats-table-${set}`;
  };

  return (
    <div className="chats-table-wrap">
      <table className="chats-table">
        <thead>
          <tr>
            {table.head.map((cell, at) => (
              <th key={at} className={align(at)} scope="col">
                <RichSpans spans={spansOf(cell)} />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((row, index) => (
            <tr key={index}>
              {row.map((cell, at) => (
                <td key={at} className={align(at)}>
                  <RichSpans spans={spansOf(cell)} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The pieces of one line, each drawn as what the parser said it was. */
function RichSpans({ spans }: { spans: RichSpan[] }) {
  return (
    <>
      {spans.map((span, index) =>
        span.kind === "code" ? (
          <code key={index}>{span.text}</code>
        ) : span.kind === "strong" ? (
          <strong key={index}>{span.text}</strong>
        ) : span.kind === "em" ? (
          <em key={index}>{span.text}</em>
        ) : span.kind === "link" ? (
          <RichLink key={index} text={span.text} href={span.href} />
        ) : (
          <span key={index}>{span.text}</span>
        ),
      )}
    </>
  );
}

/**
 * A link in an answer, opened by the OS rather than by this window.
 *
 * A `<button>` and not an `<a href>`, for the reason `lib/vscode.ts` gives at length: an external
 * URL from inside a webview is handled differently per platform and can simply be swallowed, while
 * the opener plugin crosses to the Rust side and asks the OS the way any other program would. It
 * also means no URL from a transcript is ever an `href` in this document.
 *
 * The scheme was already checked in the parser — a `javascript:` URL never became a link span at
 * all — and the plugin's own scope checks it again on the far side. The address is in the `title`
 * because a link whose destination you cannot see before pressing it is a link you should not
 * press, and this text came from a model.
 */
function RichLink({ text, href }: { text: string; href: string }) {
  return (
    <button
      type="button"
      className="chats-rich-link"
      title={href}
      /* Spelled out: the visible text is a phrase from a sentence, and a control whose whole
         accessible name is "calendário gregoriano" announces a noun rather than something that
         opens a browser. */
      aria-label={`Open ${href}`}
      onClick={() => {
        void openUrl(href).catch(() => {
          // Refused by the plugin's scope, or nothing on this machine claims the scheme. The
          // address is in the tooltip either way, which is the honest remainder of the request.
        });
      }}
    >
      {text}
    </button>
  );
}

/** One line of prose, drawn as the shape the parser found. */
function RichLineOut({ line }: { line: RichLine }) {
  if (line.kind === "blank") {
    // The paragraph break somebody typed. An empty `<p>` has no height, which is how every gap in
    // every answer was silently dropped and two thoughts came out as one.
    return <p className="chats-rich-gap" aria-hidden="true" />;
  }
  if (line.kind === "rule") {
    return <hr className="chats-rich-rule" />;
  }
  if (line.kind === "heading") {
    return (
      <p
        className={`chats-rich-heading chats-rich-heading-${Math.min(line.level, 4)}`}
      >
        <RichSpans spans={line.spans} />
      </p>
    );
  }
  if (line.kind === "quote") {
    return (
      <p className="chats-rich-quote">
        <RichSpans spans={line.spans} />
      </p>
    );
  }
  if (line.kind === "bullet") {
    return (
      <p
        className={`chats-rich-bullet chats-rich-depth-${Math.min(line.depth, 3)}`}
      >
        {/* The author's own marker, never renumbered — see `Line.marker`. A dash becomes a
            bullet because a dash is not a character anybody meant to read; a `1.` stays a `1.`
            because it is. */}
        <span className="chats-rich-marker" aria-hidden="true">
          {line.marker ?? "•"}
        </span>
        {/* One flex item, not one per span. The row exists to hang the marker beside the text;
            left unwrapped, every word and every `code` chip became its own flex item — gapped
            apart and shrinkable on its own, so `budget_usd` was squeezed until it broke mid-name
            and stacked vertically. Seen in the app, in a bulleted answer. */}
        <span className="chats-rich-bullet-text">
          <RichSpans spans={line.spans} />
        </span>
      </p>
    );
  }
  return (
    <p className="chats-rich-line">
      <RichSpans spans={line.spans} />
    </p>
  );
}

/**
 * What a person said, drawn the way a question is.
 *
 * Verbatim, and not through `Rich`: their half is not markdown and is not read as any. Somebody who
 * types two asterisks meant two asterisks, and a message redrawn as bold is a message they did not
 * send. `note` is the small line under it ("said while it was working"); `children` sit beside the
 * words, inside the same bubble (the "put it back" pencil, the pictures sent with it).
 */
export function AskedBubble({
  text,
  note,
  children,
}: {
  text: string;
  note?: string;
  children?: ReactNode;
}) {
  return (
    <div className="chats-turn-said">
      <p className="chats-turn-asked">{text}</p>
      {note !== undefined && <p className="chats-turn-said-now-note">{note}</p>}
      {children}
    </div>
  );
}

/** The model's half of a turn: its words, through `Rich`. */
export function AnswerBlock({ text }: { text: string }) {
  return (
    <div className="chats-turn-answer">
      <Rich text={text} />
    </div>
  );
}

export type ListedMessage = {
  key: string;
  role: "person" | "agent" | "action" | "notice";
  text: string;
  failed?: boolean;
  onRetry?: () => void;
};

/**
 * A plain run of messages: the person's verbatim, the agent's through `Rich`, and what the agent
 * did or the system noted as one discreet line. Router-free and data-free, so a panel and the
 * chats page can both draw a thread with it.
 */
export function MessageList({ messages }: { messages: ListedMessage[] }) {
  return (
    <ul className="chats-message-list">
      {messages.map((message) => (
        <li key={message.key} className="chats-message">
          {message.role === "person" ? (
            <AskedBubble text={message.text} />
          ) : message.role === "agent" ? (
            <AnswerBlock text={message.text} />
          ) : (
            <p className="chats-turn-who">{message.text}</p>
          )}
          {message.failed === true && (
            <p className="chats-turn-said-now-note" role="status">
              not sent
              {message.onRetry !== undefined && (
                <>
                  {" "}
                  <button
                    type="button"
                    className="chats-rich-link"
                    onClick={message.onRetry}
                  >
                    Retry
                  </button>
                </>
              )}
            </p>
          )}
        </li>
      ))}
    </ul>
  );
}
