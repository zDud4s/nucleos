/**
 * The one place the shell asks which operating system it runs on, and only for words on screen.
 *
 * Read from the user agent because it is the one fact every webview reports: WebView2 says
 * "Windows NT", WKWebView "Macintosh", WebKitGTK "Linux". Nothing here changes behaviour; a wrong
 * guess costs an example path, never a feature. The user agent is a parameter so tests can pass one.
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
