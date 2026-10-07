import { describe, expect, it } from "vitest";
import { RecordParser } from "./liveRecords";

/** One wire record: a type byte, a u32 big-endian length, then the body. */
function record(type: string, body: Uint8Array): Uint8Array {
  const out = new Uint8Array(5 + body.length);
  out[0] = type.charCodeAt(0);
  new DataView(out.buffer).setUint32(1, body.length, false);
  out.set(body, 5);
  return out;
}

function join(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

const text = (s: string) => new TextEncoder().encode(s);

describe("live records", () => {
  it("parses records split across chunks", () => {
    const jpeg = new Uint8Array([0xff, 0xd8, 1, 2, 3, 0xff, 0xd9]);
    const wire = join(record("F", jpeg), record("E", text('{"reason":"wheel"}')));

    // Feed it one byte at a time: the worst split there is, through the length too.
    const parser = new RecordParser();
    const seen = [];
    for (const byte of wire) seen.push(...parser.push(new Uint8Array([byte])));

    expect(seen).toHaveLength(2);
    expect(seen[0]).toEqual({ kind: "frame", jpeg });
    expect(seen[1]).toEqual({ kind: "end", reason: "wheel" });

    // And two records in one chunk come out together, in order.
    const both = new RecordParser().push(wire);
    expect(both.map((one) => one.kind)).toEqual(["frame", "end"]);
  });

  it("an unknown end reason reads as gone", () => {
    const parser = new RecordParser();
    expect(parser.push(record("E", text('{"reason":"lunch"}')))).toEqual([{ kind: "end", reason: "gone" }]);
    // Unparseable body, and an unknown type byte, both end as gone rather than throwing.
    expect(parser.push(record("E", text("not json")))).toEqual([{ kind: "end", reason: "gone" }]);
    expect(parser.push(record("Z", new Uint8Array([1])))).toEqual([{ kind: "end", reason: "gone" }]);
  });

  const META = {
    frameWidth: 800,
    frameHeight: 600,
    deviceWidth: 1280,
    deviceHeight: 720,
    offsetTop: 12.5,
    pageScaleFactor: 1.25,
    scrollOffsetX: 0,
    scrollOffsetY: 40,
  };

  it("parses an M record into frame metadata", () => {
    const parser = new RecordParser();
    expect(parser.push(record("M", text(JSON.stringify(META))))).toEqual([{ kind: "meta", meta: META }]);
  });

  it("parses P prompt records of every kind and a resolved one", () => {
    const parser = new RecordParser();
    const prompts = [
      { id: "p1", kind: "dialog", dialogType: "prompt", message: "Name?", defaultPrompt: "x" },
      {
        id: "p2",
        kind: "select",
        options: [
          { value: "a", label: "A", selected: false },
          { value: "b", label: "B", selected: true },
        ],
        multiple: false,
      },
      { id: "p3", kind: "file", multiple: true, accept: "image/*" },
      { id: "p4", kind: "auth", origin: "https://example.test", realm: "staff" },
    ];
    for (const prompt of prompts) {
      expect(parser.push(record("P", text(JSON.stringify(prompt))))).toEqual([{ kind: "prompt", prompt }]);
    }
    const resolved = { id: "p1", kind: "dialog", resolved: true };
    expect(parser.push(record("P", text(JSON.stringify(resolved))))).toEqual([{ kind: "prompt", prompt: resolved }]);
  });

  it("a malformed M or P body ends as gone", () => {
    const gone = [{ kind: "end", reason: "gone" }];
    const bad = [
      record("M", text(JSON.stringify({ ...META, offsetTop: undefined }))),
      record("M", text('{"frameWidth":800}')),
      record("M", text(JSON.stringify({ ...META, pageScaleFactor: "1.25" }))),
      record("M", text("not json")),
      record("P", text(JSON.stringify({ kind: "dialog", dialogType: "alert", message: "m", defaultPrompt: "" }))),
      record("P", text(JSON.stringify({ id: "p1", kind: "teleport" }))),
      record("P", text(JSON.stringify({ id: "p1", kind: "select", options: "nope", multiple: false }))),
      record("P", text("not json")),
    ];
    for (const one of bad) expect(new RecordParser().push(one)).toEqual(gone);
  });
});
