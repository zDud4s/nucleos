import { describe, expect, it } from "vitest";
import { IDEMPOTENCY_HEADER, idempotencyHeaders, newIdempotencyKey, newIntent } from "./idempotency";

describe("idempotency", () => {
  it("mints a distinct key per intent", () => {
    expect(newIdempotencyKey()).not.toBe(newIdempotencyKey());
    expect(newIntent().key).not.toBe(newIntent().key);
  });
  it("keeps one key for the life of an intent", () => {
    const intent = newIntent();
    expect(idempotencyHeaders(intent.key)).toEqual({ [IDEMPOTENCY_HEADER]: intent.key });
  });
});
