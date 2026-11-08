//go:build browsergate

// §spec pilar-de-browser

package gate_test

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/chrome"
	"nucleosbrowser/fence"
	"nucleosbrowser/launch"
)

// TestBrowserWorksOverThePipeAndOpensNoDevToolsPort: the browser is driven over the DevTools pipe, so
// no DevToolsActivePort file is written and no debugging port is on the command line.
func TestBrowserWorksOverThePipeAndOpensNoDevToolsPort(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>pipe page</title><h1>pipe</h1>`)
	}))
	t.Cleanup(server.Close)

	policy := fence.Policy{Profile: fence.Ephemeral, Loopback: []string{server.URL}}
	proxy, err := fence.NewProxy(policy)
	if err != nil {
		t.Fatalf("proxy: %v", err)
	}
	t.Cleanup(func() { _ = proxy.Close() })

	profile := t.TempDir()
	opts := launch.Options{
		ExecutablePath: chromium(t),
		ProfileDir:     profile,
		Mode:           browser.ModeAgent,
		ProxyAddr:      proxy.Addr(),
	}
	process, err := launch.Start(context.Background(), opts, 45*time.Second)
	if err != nil {
		t.Fatalf("launching chromium: %v", err)
	}
	t.Cleanup(process.Stop)

	conn := cdp.NewPipe(process.Pipe())
	t.Cleanup(func() { _ = conn.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	driver, err := chrome.Connect(ctx, conn, policy)
	if err != nil {
		t.Fatalf("attaching the fence: %v", err)
	}
	if _, err := driver.Open(ctx, browser.OpenRequest{URL: server.URL}); err != nil {
		t.Fatalf("open: %v", err)
	}

	if _, err := os.Stat(filepath.Join(profile, "DevToolsActivePort")); !os.IsNotExist(err) {
		t.Errorf("DevToolsActivePort exists or stat failed unexpectedly: %v", err)
	}
	args, err := launch.Args(opts)
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	for _, arg := range args {
		if strings.HasPrefix(arg, "--remote-debugging-port") {
			t.Errorf("a debugging port argument is present: %s", arg)
		}
	}

	raw, err := conn.Call(ctx, cdp.BrowserSession, "Browser.getVersion", nil)
	if err != nil {
		t.Fatalf("Browser.getVersion: %v", err)
	}
	var version struct {
		Product string `json:"product"`
	}
	if err := json.Unmarshal(raw, &version); err != nil {
		t.Fatalf("version payload: %v", err)
	}
	if !strings.HasPrefix(version.Product, "Chrome") && !strings.HasPrefix(version.Product, "HeadlessChrome") {
		t.Errorf("product = %q, want a Chrome product", version.Product)
	}
}
