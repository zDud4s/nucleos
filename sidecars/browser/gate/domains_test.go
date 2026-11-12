//go:build browsergate

// §spec pilar-de-browser

package gate_test

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"nucleosbrowser/cdp"
)

// This file exists because a fake browser answers everything.
//
// cdptest replies to any method it has no handler for, which is what makes the unit tests fast and
// is also how a call that does not exist in real Chrome passes them. It happened: the sweep of spec
// §5.8 was written against `ServiceWorker.enable` on the BROWSER session, the fake said yes, and the
// first run of this group answered `'ServiceWorker.enable' wasn't found (-32601)`.
//
// So the availability of every domain the fence hangs on is measured here, against the real browser,
// on the session the fence actually uses. A method that moves is then a named failure rather than a
// silent one.

type availability struct {
	method  string
	params  map[string]any
	session string // "browser" or "page"
	why     string
}

func TestTheDomainsTheFenceHangsOnExistWhereItHangsThem(t *testing.T) {
	conn := control(t)
	page := openIn(t, conn, "about:blank")
	time.Sleep(300 * time.Millisecond)

	cases := []availability{
		{"Fetch.enable", map[string]any{"patterns": []map[string]any{{"urlPattern": "*"}}, "handleAuthRequests": true}, "browser",
			"the interception itself; spec §6.2 and the spike's central finding"},
		{"Target.setAutoAttach", map[string]any{"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}, "browser",
			"closes the TOCTOU window of spec §5.4"},
		{"Browser.setDownloadBehavior", map[string]any{"behavior": "deny"}, "browser",
			"spec §11 test 8"},
		{"Target.getTargetInfo", nil, "browser",
			"the final url, which spec §5.3's conjunction needs"},
		{"ServiceWorker.enable", nil, "page",
			"the sweep of spec §5.8 — NOT available on the browser session, measured"},
		{"Page.enable", nil, "page", "navigation"},
		{"Accessibility.getFullAXTree", nil, "page", "the snapshot"},
	}

	for _, testCase := range cases {
		t.Run(testCase.method+"/"+testCase.session, func(t *testing.T) {
			target := cdp.BrowserSession
			if testCase.session == "page" {
				target = page
			}
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			defer cancel()

			params := testCase.params
			if testCase.method == "Target.getTargetInfo" {
				params = map[string]any{"targetId": targetIDOf(t, conn, page)}
			}
			if _, err := conn.Call(ctx, target, testCase.method, params); err != nil {
				var protocolErr *cdp.ProtocolError
				if ok := asProtocol(err, &protocolErr); ok && protocolErr.Code == -32601 {
					t.Fatalf("%s does not exist on the %s session, and the fence needs it there: %s",
						testCase.method, testCase.session, testCase.why)
				}
				t.Logf("%s answered %v (not a missing-method error, so the domain is there)", testCase.method, err)
			}
		})
	}
}

// TestServiceWorkerIsNotOnTheBrowserSession records the measurement that cost a gate run, so that a
// future change moving the sweep back to the browser session fails here with the reason attached.
func TestServiceWorkerIsNotOnTheBrowserSession(t *testing.T) {
	conn := control(t)
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	_, err := conn.Call(ctx, cdp.BrowserSession, "ServiceWorker.enable", nil)
	if err == nil {
		t.Fatal("ServiceWorker.enable now works on the browser session; the sweep can be simplified, " +
			"and the comment in chrome/serviceworker.go explaining why it is not there is stale")
	}
	var protocolErr *cdp.ProtocolError
	if !asProtocol(err, &protocolErr) || protocolErr.Code != -32601 {
		t.Fatalf("expected a missing-method error, got %v", err)
	}
}

func asProtocol(err error, target **cdp.ProtocolError) bool {
	if converted, ok := err.(*cdp.ProtocolError); ok {
		*target = converted
		return true
	}
	return false
}

func targetIDOf(t *testing.T, conn *cdp.Conn, session cdp.SessionID) string {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	result, err := conn.Call(ctx, session, "Target.getTargetInfo", nil)
	if err != nil {
		t.Fatalf("target info: %v", err)
	}
	var payload struct {
		TargetInfo struct {
			TargetID string `json:"targetId"`
		} `json:"targetInfo"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		t.Fatalf("target info: %v", err)
	}
	return payload.TargetInfo.TargetID
}

// TestThePersonPromptDomainsExistWhereTheyAreCalled. Person mode intercepts the file chooser and finds
// the node under a press, both on the page session; a fake browser answers anything, so the real one is
// asked here.
func TestThePersonPromptDomainsExistWhereTheyAreCalled(t *testing.T) {
	conn := control(t)
	page := openIn(t, conn, "about:blank")
	time.Sleep(300 * time.Millisecond)

	for _, testCase := range []availability{
		{"Page.setInterceptFileChooserDialog", map[string]any{"enabled": false}, "page",
			"the file prompt of a person's turn"},
		{"DOM.getNodeForLocation", map[string]any{"x": 1, "y": 1, "includeUserAgentShadowDOM": true}, "page",
			"the select prompt of a person's turn"},
	} {
		t.Run(testCase.method+"/"+testCase.session, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			defer cancel()
			if _, err := conn.Call(ctx, page, testCase.method, testCase.params); err != nil {
				var protocolErr *cdp.ProtocolError
				if ok := asProtocol(err, &protocolErr); ok && protocolErr.Code == -32601 {
					t.Fatalf("%s does not exist on the %s session, and person mode needs it there: %s",
						testCase.method, testCase.session, testCase.why)
				}
				t.Logf("%s answered %v (not a missing-method error, so the method is there)", testCase.method, err)
			}
		})
	}
}
