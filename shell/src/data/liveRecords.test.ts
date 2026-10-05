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
});
