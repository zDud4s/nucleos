/**
 * The one place the shell asks which operating system it runs on, for words on screen and for the
 * Windows-only removal of the old sessions list (and its "Open real window") from the Web page.
 *
 * Read from the user agent because it is the one fact every webview reports: WebView2 says
 * "Windows NT", WKWebView "Macintosh", WebKitGTK "Linux". Apart from that sessions-list gate, nothing here
 * changes behaviour; a wrong guess costs an example path. The user agent is a parameter so tests can pass one.
 */
export function isMac(userAgent: string = navigator.userAgent): boolean {
  return /Mac/.test(userAgent);
}

export function isWindows(userAgent: string = navigator.userAgent): boolean {
  return /Windows/.test(userAgent);
}

/** An example absolute path in the spelling this machine uses. */
export function projectPathPlaceholder(userAgent: string = navigator.userAgent): string {
  return isWindows(userAgent) ? "C:\path\to\the\project" : "/path/to/the/project";
}
