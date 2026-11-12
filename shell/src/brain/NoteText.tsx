import Markdown, { type Components } from "react-markdown";
import { openUrl } from "@tauri-apps/plugin-opener";
import "./NoteText.css";

/**
 * A note's text rendered as markdown.
 *
 * What is deliberately NOT rendered, because a note can arrive from a channel and is not trusted:
 * raw HTML (`skipHtml`, and no `rehype-raw`), images (nothing is fetched on render; the alt text
 * stays as text), and any inline style (this component emits none, which keeps the CSP gate green).
 * react-markdown's default URL transform already drops `javascript:` and similar schemes.
 */

/** Schemes the opener plugin's default scope accepts; anything else is shown as text only. */
const OPENABLE = /^(https?:|mailto:|tel:)/i;

/**
 * A link in a note, opened by the OS rather than by this window.
 *
 * A `<button>` and not an `<a href>`, for the reason `chats/MessageList.tsx` and `lib/vscode.ts`
 * give: an external URL from inside a webview is handled differently per platform and can be
 * swallowed, while the opener plugin asks the OS. No URL from a note is ever an `href` here. The
 * address is in the `title` so it can be read before pressing, since a note may come from a channel.
 */
function NoteLink({ href, children }: { href: string | undefined; children: React.ReactNode }) {
  // A link whose URL was stripped (a `javascript:` target) or is not openable is plain text.
  if (!href || !OPENABLE.test(href)) return <span>{children}</span>;
  return (
    <button
      type="button"
      className="brain-note-link"
      title={href}
      onClick={() => {
        void openUrl(href).catch(() => {
          // Refused by the plugin's scope, or nothing claims the scheme: the address stays in the
          // tooltip, which is the honest remainder of the request.
        });
      }}
    >
      {children}
    </button>
  );
}

const COMPONENTS: Components = {
  a: ({ node: _node, children, href }) => <NoteLink href={href}>{children}</NoteLink>,
  // No `<img>` is ever emitted; an image-only note keeps its alt text instead of rendering empty.
  img: ({ node: _node, alt }) => (alt ? <span>{alt}</span> : null),
};

export function NoteText({ text }: { text: string }) {
  return (
    <div className="brain-text brain-markdown">
      <Markdown skipHtml components={COMPONENTS}>
        {text}
      </Markdown>
    </div>
  );
}
