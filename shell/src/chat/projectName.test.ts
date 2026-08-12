import { describe, expect, it } from "vitest";

import { projectName } from "./projectName";

describe("projectName", () => {
  it("names a directory by its last segment", () => {
    expect(projectName("C:\\Projects\\nucleos-canvas")).toBe("nucleos-canvas");
  });

  /// The daemon reads these paths out of transcripts written by whichever machine had the session,
  /// so both separators arrive here and neither is the odd one out.
  it("reads both separators", () => {
    expect(projectName("/home/dudas/projects/nucleos")).toBe("nucleos");
  });

  it("ignores a trailing separator rather than answering with nothing", () => {
    expect(projectName("C:\\Projects\\nucleos\\")).toBe("nucleos");
  });

  /// A path that is only separators has no last segment. Answering with the empty string would put
  /// a blank chip in the list; answering with the path at least shows something true.
  it("falls back to the whole path when there is no segment to take", () => {
    expect(projectName("/")).toBe("/");
  });
});
