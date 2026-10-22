import Markdown, { type Components } from "react-markdown";
import "./NoteText.css";

/**
 * A note's text rendered as markdown.
 *
 * What is deliberately NOT rendered, because a note can arrive from a channel and is not trusted:
 * raw HTML (`skipHtml`, and no `rehype-raw`), images (nothing is fetched on render), and any
 * inline style (this component emits none, which keeps the CSP gate green). Links open outside
 * the webview with `noreferrer noopener`; react-markdown's default URL transform already drops
 * `javascript:` and similar schemes.
 */
const COMPONENTS: Components = {
  a: ({ node: _node, children, href, title }) => (
    <a href={href} title={title} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
  img: () => null,
};

export function NoteText({ text }: { text: string }) {
  return (
    <div className="brain-text brain-markdown">
      <Markdown skipHtml disallowedElements={["img"]} components={COMPONENTS}>
        {text}
      </Markdown>
    </div>
  );
}
