import { openUrl } from "@tauri-apps/plugin-opener";

/**
 * The door to the editor.
 *
 * The Código mode is a review surface, not an IDE: its subject is the run, and
 * the place where code is *changed* is VS Code, already open on the same machine
 * on the same repository. This module is the whole seam between the two, and it
 * is deliberately one file with one exported URL and one exported call — a seam
 * you can read in a minute is a seam nobody routes around.
 *
 * Why a link and not an embedded editor, recorded here because the question
 * comes back: Monaco gives writing without working (no rust-analyzer, no cargo,
 * no terminal — you change a line and go to VS Code anyway to check it), and
 * code-server charges twice, in installs and in a CSP that would have to open
 * for localhost. And the argument that settles it is not about the desktop at
 * all: this app approves work from Telegram and e-mail too, where an embedded
 * editor is useless and a good diff reader is exactly right.
 */

/**
 * Where VS Code should open, as its URL handler spells it.
 *
 * Kept pure and separate from {@link openInVscode} because the mistakes here are
 * all in the string — a drive letter's colon, a space in a folder name, a
 * separator that came back the wrong way round — and a string is testable
 * without a webview, which the call is not.
 */
export function vscodeUrl(absolutePath: string, line: number | null): string {
  const forwardSlashed = absolutePath.replace(/\\/g, "/");
  // `file` is the URL's authority and the path follows it, so the slash in
  // `vscode://file/` is already the path's first separator — a POSIX path that
  // brings its own leading slash would double it. That is not cosmetic: VS
  // Code's handler passes the URL's `fsPath` to `URI.file()`, and
  // `URI.file("//home/x/a.rs")` reads the leading `//` as a UNC share, so the
  // host becomes `home` and the file is never asked for on this machine. One
  // slash gives `file:///home/x/a.rs`, which is the local file. A Windows path
  // never reaches this branch — its drive letter follows the single slash
  // already — which is why one shape cannot serve both.
  const afterAuthority = forwardSlashed.startsWith("/")
    ? forwardSlashed.slice(1)
    : forwardSlashed;
  // `encodeURI` is the right encoder — it leaves `/` and the drive letter's `:`
  // alone, which a component encoder would destroy — but it also leaves `#` and
  // `?` alone, and those two end the path early rather than sitting in it.
  const encoded = encodeURI(afterAuthority).replace(/#/g, "%23").replace(/\?/g, "%3F");
  // A line number is a place in a file and there is no zeroth line. Anything
  // that cannot be one is dropped, which opens the file — the honest remainder
  // of the request — rather than landing the cursor somewhere nobody meant.
  const at = line !== null && Number.isInteger(line) && line > 0 ? `:${line}` : "";
  return `vscode://file/${encoded}${at}`;
}

/**
 * Ask the OS to hand this path to VS Code.
 *
 * Through the opener plugin rather than an `<a href>`, and the difference is not
 * stylistic: an external protocol from inside the webview is handled differently
 * per platform and can simply be swallowed, while the plugin crosses to the Rust
 * side and asks the OS the way any other program would.
 *
 * `opener:default` does NOT cover this. Its `allow-default-urls` says so in its
 * own description — `mailto:`, `tel:`, `http://`, `https://` and nothing else —
 * so `src-tauri/capabilities/default.json` adds a scope entry, and it is
 * deliberately `vscode://file/*` rather than `vscode://*`. The scheme does more
 * than open files: `vscode://extension/…` installs an extension and
 * `vscode://vscode.git/clone?url=…` clones a repository. Only one of those three
 * is a thing this window should be able to ask for, and the pattern is where
 * that is said. (`glob::Pattern` matches with separators non-literal here, so the
 * single `*` does cross the slashes of a path — the plugin's own `http://*`
 * relies on the same.)
 *
 * Checked rather than reasoned about, since the pattern is the whole fence: run
 * against `glob` 0.3.3, the crate `tauri-plugin-opener` builds the entry with,
 * `vscode://file/*` admits every URL {@link vscodeUrl} produces — drive letter,
 * line suffix, percent-encoded space — and refuses both
 * `vscode://extension/…` and `vscode://vscode.git/clone?url=…`. The scheme is
 * registered on this machine under `HKCU\SOFTWARE\Classesscode`, pointing at
 * `Code.exe --open-url`, so the OS has somewhere to take it.
 *
 * Rejects when the scheme is outside the opener's allowed scope, or when nothing
 * on this machine claims `vscode://`. The caller decides what to say about it —
 * "VS Code is not installed" and "the shell is not allowed to ask" want
 * different sentences, and neither of them is a stack trace.
 */
export function openInVscode(absolutePath: string, line: number | null): Promise<void> {
  return openUrl(vscodeUrl(absolutePath, line));
}
