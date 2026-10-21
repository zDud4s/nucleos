// @vitest-environment node
import { describe, expect, it } from "vitest";
import { NdjsonSplitter, parseEvent } from "./ndjson";
import { node } from "./test-fixtures";

const enc = new TextEncoder();

describe("NdjsonSplitter", () => {
  it("holds a line until its newline arrives, across chunks", () => {
    const split = new NdjsonSplitter();
    expect(split.push(enc.encode('{"t":"heart'))).toEqual([]);
    expect(split.push(enc.encode('beat"}\n{"t":"heartbeat"}'))).toEqual(['{"t":"heartbeat"}']);
    expect(split.push(enc.encode("\n"))).toEqual(['{"t":"heartbeat"}']);
  });

  it("does not split a multi-byte character that straddles two chunks", () => {
    const bytes = enc.encode('{"t":"lines","node_id":"a","lines":[{"kind":"text","text":"café ✓"}]}\n');
    const cut = bytes.indexOf(0xc3) + 1; // between the two bytes of "é"
    const split = new NdjsonSplitter();
    expect(split.push(bytes.slice(0, cut))).toEqual([]);
    const [only] = split.push(bytes.slice(cut));
    expect(JSON.parse(only).lines[0].text).toBe("café ✓");
  });

  it("tolerates CRLF and blank lines", () => {
    const split = new NdjsonSplitter();
    expect(split.push(enc.encode('{"t":"heartbeat"}\r\n\r\n'))).toEqual(['{"t":"heartbeat"}']);
  });
});

describe("parseEvent", () => {
  it("parses a known event", () => {
    expect(parseEvent('{"t":"removed","node_id":"a:1"}')).toEqual({ t: "removed", node_id: "a:1" });
  });
  it("drops garbage and unknown discriminants instead of throwing", () => {
    expect(parseEvent("not json")).toBeNull();
    expect(parseEvent('{"t":"from_the_future"}')).toBeNull();
    expect(parseEvent("42")).toBeNull();
  });
  it("drops a node without capabilities and any event that lacks what the store reads", () => {
    const { capabilities: _gone, ...bare } = node("a:1");
    expect(parseEvent(JSON.stringify({ t: "node", node: bare }))).toBeNull();
    expect(parseEvent('{"t":"node"}')).toBeNull();
    expect(parseEvent('{"t":"snapshot"}')).toBeNull();
    expect(parseEvent('{"t":"metrics"}')).toBeNull();
    expect(parseEvent('{"t":"lines","node_id":"a"}')).toBeNull();
    expect(parseEvent('{"t":"removed"}')).toBeNull();
  });
  it("normalises a snapshot from a skewed daemon: missing metrics/lines, a bad node, a bad line", () => {
    const { capabilities: _gone, ...bare } = node("bad");
    const raw = { t: "snapshot", nodes: [node("a:1"), bare, 7], lines: { "a:1": [{ kind: "text", text: "ok" }, { kind: "text" }], x: "nope" } };
    expect(parseEvent(JSON.stringify(raw))).toEqual({
      t: "snapshot",
      nodes: [node("a:1")],
      metrics: [],
      lines: { "a:1": [{ kind: "text", text: "ok" }] },
    });
  });
});
