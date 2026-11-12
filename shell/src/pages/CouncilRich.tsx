import { blocks, lines, spans as spansOf, type Block, type Line, type Span } from "../lib/rich";

/**
 * A seat's answer, or the chairman's synthesis, drawn as the markdown it was
 * written in.
 *
 * The parser is `lib/rich.ts`, the same one the conversations use, and this is
 * a deliberately smaller renderer over it: prose, headings, bullets, quotes,
 * fenced code and tables, with emphasis and inline code inside them. What it
 * never does is pass markup through. Every piece of text reaches React as a
 * string child, so `<img onerror=…>` in an answer is those characters on the
 * page — a model's answer is not trusted input, and a renderer that rendered
 * HTML would run whatever it said.
 *
 * Links are drawn as their text with the address in the title, not as anchors
 * or buttons. `Chats.tsx` opens them through the OS opener plugin; a council
 * answer is read rather than acted on, and this keeps the page free of any
 * control that leaves the window. The address is still visible on hover,
 * because a link whose destination you cannot see is not one to trust.
 */
export function CouncilRich({ text }: { text: string }) {
  return (
    <div className="council-rich">
      {blocks(text).map((block, index) => (
        <RichBlock key={index} block={block} />
      ))}
    </div>
  );
}

function RichBlock({ block }: { block: Block }) {
  if (block.kind === "code") {
    return (
      <pre className="council-rich-code">
        <code>{block.text}</code>
      </pre>
    );
  }
  if (block.kind === "table") {
    return (
      <div className="council-rich-table-wrap">
        <table className="council-rich-table">
          <thead>
            <tr>
              {block.head.map((cell, at) => (
                <th key={at} scope="col">
                  <RichSpans spans={spansOf(cell)} />
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {block.rows.map((row, index) => (
              <tr key={index}>
                {row.map((cell, at) => (
                  <td key={at}>
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
  return (
    <>
      {lines(block.text).map((line, at) => (
        <RichLine key={at} line={line} />
      ))}
    </>
  );
}

function RichLine({ line }: { line: Line }) {
  switch (line.kind) {
    case "blank":
      // The paragraph break the author typed. An empty `<p>` has no height, so
      // it is given one in `council.css` rather than silently dropped.
      return <p className="council-rich-gap" aria-hidden="true" />;
    case "rule":
      return <hr className="council-rich-rule" />;
    case "heading":
      return (
        <p className="council-rich-heading">
          <RichSpans spans={line.spans} />
        </p>
      );
    case "quote":
      return (
        <p className="council-rich-quote">
          <RichSpans spans={line.spans} />
        </p>
      );
    case "bullet":
      return (
        <p className="council-rich-bullet">
          {/* The author's own marker, never renumbered — see `Line.marker`. */}
          <span className="council-rich-marker" aria-hidden="true">
            {line.marker ?? "•"}
          </span>
          <span>
            <RichSpans spans={line.spans} />
          </span>
        </p>
      );
    case "line":
      return (
        <p className="council-rich-line">
          <RichSpans spans={line.spans} />
        </p>
      );
  }
}

function RichSpans({ spans }: { spans: Span[] }) {
  return (
    <>
      {spans.map((span, index) => {
        switch (span.kind) {
          case "code":
            return <code key={index}>{span.text}</code>;
          case "strong":
            return <strong key={index}>{span.text}</strong>;
          case "em":
            return <em key={index}>{span.text}</em>;
          case "link":
            return (
              <span key={index} className="council-rich-link" title={span.href}>
                {span.text}
              </span>
            );
          case "plain":
            return <span key={index}>{span.text}</span>;
        }
      })}
    </>
  );
}
