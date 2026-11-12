package serve

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

var nonceRe = regexp.MustCompile(`'nonce-([^']+)'`)

func g2Dist(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	write := func(rel, content string) {
		full := filepath.Join(dir, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(full, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write("index.html", `<!doctype html><html><head><style nonce="__CSP_NONCE__"></style></head><body>app</body></html>`)
	write("assets/app.js", "console.log('app')")
	write(".secret", "hidden")
	return dir
}

func TestStaticServesIndexWithNonceAndSpaFallback(t *testing.T) {
	cfg := g2Config("http://127.0.0.1:1")
	cfg.DistDir = g2Dist(t)
	gw := g2Gateway(t, cfg)

	nonceOf := func(path string) (nonce, body string) {
		resp := g2Do(t, gw, "GET", path, "", nil)
		if resp.StatusCode != 200 {
			t.Fatalf("%s: status = %d, want 200", path, resp.StatusCode)
		}
		body = g2Body(t, resp)
		m := nonceRe.FindStringSubmatch(resp.Header.Get("Content-Security-Policy"))
		if m == nil {
			t.Fatalf("%s: no nonce in the CSP", path)
		}
		return m[1], body
	}

	n1, body := nonceOf("/")
	if strings.Contains(body, "__CSP_NONCE__") || !strings.Contains(body, `nonce="`+n1+`"`) {
		t.Fatalf("index nonce does not match the CSP nonce %q: %s", n1, body)
	}
	n2, _ := nonceOf("/")
	if n1 == n2 {
		t.Fatal("the nonce repeated across requests")
	}
	// Client-side routes (no file extension) get the index, with their own nonce.
	n3, body := nonceOf("/settings/devices")
	if !strings.Contains(body, `nonce="`+n3+`"`) {
		t.Fatalf("fallback nonce mismatch: %s", body)
	}
	if _, body = nonceOf("/index.html"); strings.Contains(body, "__CSP_NONCE__") {
		t.Fatal("/index.html served the raw placeholder")
	}

	resp := g2Do(t, gw, "GET", "/assets/app.js", "", nil)
	if resp.StatusCode != 200 || g2Body(t, resp) != "console.log('app')" {
		t.Fatalf("asset: status = %d", resp.StatusCode)
	}
	for _, path := range []string{"/assets/missing.js", "/.secret", "/assets/.hidden", "/favicon.ico"} {
		if resp := g2Do(t, gw, "GET", path, "", nil); resp.StatusCode != 404 {
			t.Errorf("%s: status = %d, want 404 (a missing file is not the SPA)", path, resp.StatusCode)
		}
	}
	// A directory is not listed: it is treated as a client route.
	resp = g2Do(t, gw, "GET", "/assets", "", nil)
	if body := g2Body(t, resp); strings.Contains(body, "app.js") {
		t.Fatalf("directory listing leaked: %s", body)
	}
	if resp := g2Do(t, gw, "POST", "/", "", nil); resp.StatusCode != 405 {
		t.Errorf("POST /: status = %d, want 405", resp.StatusCode)
	}
}
