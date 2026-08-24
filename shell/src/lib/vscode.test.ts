import { describe, expect, it } from "vitest";
import { vscodeUrl } from "./vscode";

describe("vscodeUrl", () => {
  it("points at an absolute Windows path and a line", () => {
    expect(vscodeUrl("C:\\Projects\\nucleos\\core\\src\\http.rs", 2190)).toBe(
      "vscode://file/C:/Projects/nucleos/core/src/http.rs:2190",
    );
  });

  it("omits the line when there is none, rather than sending :0", () => {
    expect(vscodeUrl("C:\\Projects\\nucleos\\README.md", null)).toBe(
      "vscode://file/C:/Projects/nucleos/README.md",
    );
  });

  /**
   * Not hypothetical: this machine's home is `C:\Users\PC Multimedia`, and a
   * link that breaks at the first space would break on every path under it.
   */
  it("encodes a space without encoding the separators", () => {
    expect(vscodeUrl("C:\\Program Files\\Git\\README.md", null)).toBe(
      "vscode://file/C:/Program%20Files/Git/README.md",
    );
  });

  /**
   * `encodeURI` leaves `#` and `?` alone — they are legal URL syntax, just not
   * legal *here*: everything after them stops being the path. A file called
   * `notes#2.md` would open `notes` and lose the rest, which reads as the editor
   * ignoring the link rather than as a name being mangled.
   */
  it("encodes the two characters that would end the path early", () => {
    expect(vscodeUrl("C:\\Projects\\notes#2.md", null)).toBe(
      "vscode://file/C:/Projects/notes%232.md",
    );
    expect(vscodeUrl("C:\\Projects\\what?.md", null)).toBe(
      "vscode://file/C:/Projects/what%3F.md",
    );
  });

  /**
   * The daemon reports whatever the platform gave it, and the same shell will
   * one day read a project on a machine that spells paths the other way. The
   * doubled slash is the scheme's own shape — `vscode://file/` plus a path that
   * already begins with one — not a defect to trim.
   */
  it("leaves a POSIX path alone", () => {
    expect(vscodeUrl("/home/x/nucleos/core/src/http.rs", 12)).toBe(
      "vscode://file//home/x/nucleos/core/src/http.rs:12",
    );
  });

  /**
   * A line number is a place in a file, and there is no zeroth line. Passing one
   * through would land the cursor somewhere the caller did not mean; dropping it
   * opens the file, which is the honest remainder of the request.
   */
  it("drops a line number that cannot be one", () => {
    expect(vscodeUrl("C:\\a\\b.md", 0)).toBe("vscode://file/C:/a/b.md");
    expect(vscodeUrl("C:\\a\\b.md", -3)).toBe("vscode://file/C:/a/b.md");
  });
});
