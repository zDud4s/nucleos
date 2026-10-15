//go:build browsergate

// §spec browser-com-painel

package gate_test

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/chrome"
	"nucleosbrowser/fence"
	"nucleosbrowser/launch"
)

// TestAVisibleSessionShowsItsPanel: the window a person opens "with agent" carries the chat panel
// on the right of the page itself. Two pages, because the second is what google.com does: a CSP
// that requires Trusted Types, under which a script that writes the DOM the old way is refused.
func TestAVisibleSessionShowsItsPanel(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		if r.URL.Path == "/tt" {
			w.Header().Set("Content-Security-Policy", "require-trusted-types-for 'script'; trusted-types 'none'")
		}
		fmt.Fprintf(w, `<!doctype html><title>%s</title><h1>%s</h1>`, r.URL.Path, r.URL.Path)
	}))
	t.Cleanup(server.Close)

	for _, path := range []string{"/plain", "/tt"} {
		t.Run(strings.TrimPrefix(path, "/"), func(t *testing.T) {
			policy := fence.Policy{Profile: fence.Ephemeral, Loopback: []string{server.URL}}
			proxy, err := fence.NewProxy(policy)
			if err != nil {
				t.Fatalf("proxy: %v", err)
			}
			t.Cleanup(func() { _ = proxy.Close() })
			process, err := launch.Start(context.Background(), launch.Options{
				ExecutablePath: chromium(t),
				ProfileDir:     t.TempDir(),
				Mode:           browser.ModeAgent,
				ProxyAddr:      proxy.Addr(),
			}, 45*time.Second)
			if err != nil {
				t.Fatalf("launching chromium: %v", err)
			}
			t.Cleanup(process.Stop)

			ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
			defer cancel()
			driver, err := chrome.Connect(ctx, dial(t, process.Port), policy)
			if err != nil {
				t.Fatalf("attaching the fence: %v", err)
			}
			driver.MakeVisible(proxy.SetPerson)
			if _, err := driver.Open(ctx, browser.OpenRequest{URL: server.URL + path}); err != nil {
				t.Fatalf("open: %v", err)
			}

			watcher := dial(t, process.Port)
			session := attachTo(t, watcher, server.URL+path)
			deadline := time.Now().Add(10 * time.Second)
			for {
				got := evaluate(t, watcher, session, `!!document.querySelector("nucleos-panel")`)
				if string(got) == "true" {
					return
				}
				if time.Now().After(deadline) {
					t.Fatalf("no panel on %s after 10s", path)
				}
				time.Sleep(200 * time.Millisecond)
			}
		})
	}
}

// attachTo attaches a second connection to the page already showing url.
func attachTo(t *testing.T, conn *cdp.Conn, url string) cdp.SessionID {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	raw, err := conn.Call(ctx, cdp.BrowserSession, "Target.getTargets", nil)
	if err != nil {
		t.Fatalf("targets: %v", err)
	}
	var targets struct {
		TargetInfos []struct {
			TargetID string `json:"targetId"`
			Type     string `json:"type"`
			URL      string `json:"url"`
		} `json:"targetInfos"`
	}
	if err := json.Unmarshal(raw, &targets); err != nil {
		t.Fatalf("targets: %v", err)
	}
	for _, target := range targets.TargetInfos {
		if target.Type != "page" || target.URL != url {
			continue
		}
		attached, err := conn.Call(ctx, cdp.BrowserSession, "Target.attachToTarget", map[string]any{
			"targetId": target.TargetID,
			"flatten":  true,
		})
		if err != nil {
			t.Fatalf("attaching: %v", err)
		}
		var session struct {
			SessionID cdp.SessionID `json:"sessionId"`
		}
		if err := json.Unmarshal(attached, &session); err != nil {
			t.Fatalf("session id: %v", err)
		}
		return session.SessionID
	}
	t.Fatalf("no page at %s among %+v", url, targets.TargetInfos)
	return ""
}
