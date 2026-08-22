import { describe, expect, it } from "vitest";
import { isPicture, splitDataUrl } from "./picture";

describe("isPicture", () => {
  it("takes the four the API can carry", () => {
    for (const type of ["image/png", "image/jpeg", "image/gif", "image/webp"]) {
      expect(isPicture(new File([""], "x", { type }))).toBe(true);
    }
  });

  // Refused here rather than accepted, sent, and refused at the far end after the person has
  // waited for it.
  it("refuses anything the API would not take", () => {
    for (const type of ["image/bmp", "image/svg+xml", "application/pdf", "text/plain", ""]) {
      expect(isPicture(new File([""], "x", { type }))).toBe(false);
    }
  });
});

describe("splitDataUrl", () => {
  it("separates the media type from the payload", () => {
    expect(splitDataUrl("data:image/png;base64,aGVsbG8=")).toEqual({
      media_type: "image/png",
      data: "aGVsbG8=",
    });
  });

  // The failure nothing would notice: a payload still carrying its prefix is valid base64 of the
  // wrong bytes, so it reaches the model as a picture that will not decode rather than as an error.
  it("leaves no prefix in the payload", () => {
    const split = splitDataUrl("data:image/png;base64,aGVsbG8=");

    expect(split?.data.startsWith("data:")).toBe(false);
    expect(split?.data).not.toContain("base64,");
  });

  it("is nothing for anything that is not a base64 data URL", () => {
    expect(splitDataUrl("https://example.test/a.png")).toBeNull();
    expect(splitDataUrl("data:image/png,notbase64")).toBeNull();
    expect(splitDataUrl("")).toBeNull();
  });

  // Base64 of a real screenshot contains newlines in some encoders, and a regex that stopped at
  // the first one would hand back a truncated picture.
  it("keeps a payload that runs across lines", () => {
    expect(splitDataUrl("data:image/png;base64,aGVs\nbG8=")?.data).toBe("aGVs\nbG8=");
  });
});
