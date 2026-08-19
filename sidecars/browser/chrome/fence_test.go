package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
	"nucleosbrowser/fence"
)

// waitForCall polls until the driver makes a call, or gives up.
//
// Polling rather than a channel because the assertion that matters is "an answer arrived at all":
// the failure this whole file exists to catch is a paused request nobody answers, and a test that
// blocked on a channel for ever would report it as a hung suite rather than a named failure.
func waitForCall(t *testing.T, fake *cdptest.Browser, method string) cdptest.Call {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		for _, call := range fake.Calls() {
			if call.Method == method {
				return call
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("%s never happened; the paused request was left unanswered: %v", method, fake.Methods())
	return cdptest.Call{}
}

func hasCall(fake *cdptest.Browser, method string) bool {
	for _, call := range fake.Calls() {
		if call.Method == method {
			return true
		}
	}
	return false
}

func pauseRequest(fake *cdptest.Browser, session string, request map[string]any) {
	fake.Emit(session, "Fetch.requestPaused", request)
}

func requestStage(url, method, resourceType string, headers map[string]any) map[string]any {
	if headers == nil {
		headers = map[string]any{}
	}
	return map[string]any{
		"requestId":    "R1",
		"frameId":      "F1",
		"resourceType": resourceType,
		"request": map[string]any{
			"url":     url,
			"method":  method,
			"headers": headers,
		},
	}
}

// TestTheFenceAnswersEveryPausedRequest is the eight gate tests' shape at the unit level, and the
// control §11 demands is built into it rather than bolted on: EVERY case asserts that an answer
// happened, so a case that expects a block cannot pass by the request being silently dropped.
//
// The spike produced a false PASS exactly this way — a paused favicon nobody answered wedged the
// renderer, and "the page did not load" looked like the fence working.
func TestTheFenceAnswersEveryPausedRequest(t *testing.T) {
	project := fence.Policy{Profile: fence.Project, Origins: []string{"https://example.org"}}
	ephemeral := fence.Policy{Profile: fence.Ephemeral}

	cases := []struct {
		name    string
		policy  fence.Policy
		paused  map[string]any
		blocked bool
	}{
		{
			name:   "a GET for a listed document passes",
			policy: project,
			paused: requestStage("https://example.org/page", "GET", "Document", nil),
		},
		{
			name:    "a POST is a form submission and does not leave",
			policy:  project,
			paused:  requestStage("https://example.org/page", "POST", "Document", nil),
			blocked: true,
		},
		{
			name:    "a DELETE from a script does not leave either",
			policy:  project,
			paused:  requestStage("https://example.org/api/thing", "DELETE", "XHR", nil),
			blocked: true,
		},
		{
			name:   "an off-list sub-resource passes: spec §5.5 is about documents",
			policy: project,
			paused: requestStage("https://cdn.example.net/app.js", "GET", "Script", nil),
		},
		{
			name:    "an off-list document does not run next to the cookies",
			policy:  project,
			paused:  requestStage("https://evil.example.net/page", "GET", "Document", nil),
			blocked: true,
		},
		{
			name:    "an off-list document in a FRAME is the same rule",
			policy:  project,
			paused:  requestStage("https://evil.example.net/frame", "GET", "Document", nil),
			blocked: true,
		},
		{
			name:   "the same document is fine in an ephemeral profile: there is nothing to spend",
			policy: ephemeral,
			paused: requestStage("https://evil.example.net/page", "GET", "Document", nil),
		},
		{
			name:   "a subdomain of a listed host is NOT the listed host",
			policy: fence.Policy{Profile: fence.Project, Origins: []string{"https://jira.example.com"}},
			paused: requestStage("https://evil.jira.example.com/x", "GET", "Document", nil),
			// Blocked, and named separately from the case above because this is the match rule
			// itself failing, not the list being short.
			blocked: true,
		},
		{
			name:    "an http document never matches an https entry",
			policy:  project,
			paused:  requestStage("http://example.org/page", "GET", "Document", nil),
			blocked: true,
		},
		{
			name:    "a file: url is not a channel this browser speaks",
			policy:  ephemeral,
			paused:  requestStage("file:///C:/Users/secrets.txt", "GET", "Document", nil),
			blocked: true,
		},
		{
			name:   "a websocket upgrade, if Fetch ever shows us one",
			policy: ephemeral,
			paused: requestStage("https://example.org/socket", "GET", "Other",
				map[string]any{"Upgrade": "websocket", "Connection": "Upgrade"}),
			blocked: true,
		},
	}

	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			fake, conn := dial(t)
			if _, err := Connect(context.Background(), conn, testCase.policy); err != nil {
				t.Fatalf("connect: %v", err)
			}
			pauseRequest(fake, "S1", testCase.paused)

			want := "Fetch.continueRequest"
			if testCase.blocked {
				want = "Fetch.failRequest"
			}
			call := waitForCall(t, fake, want)
			if call.Session != "S1" {
				t.Errorf("answered on session %q, not the one that paused", call.Session)
			}
			if other := map[bool]string{true: "Fetch.continueRequest", false: "Fetch.failRequest"}[testCase.blocked]; hasCall(fake, other) {
				t.Errorf("the request was answered twice, once with %s", other)
			}
		})
	}
}

// TestAnUnansweredContinueBecomesAFailure is the anti-wedge rule with the browser fighting back.
//
// If continueRequest is rejected and the driver just returns, the request stays paused for ever and
// the renderer hangs. That state is indistinguishable from a blocked page, which is precisely how a
// broken fence passes a test suite.
func TestAnUnansweredContinueBecomesAFailure(t *testing.T) {
	fake, conn := dial(t)
	fake.Handle("Fetch.continueRequest", func(cdptest.Call) (any, error) {
		return nil, errors.New("Invalid InterceptionId")
	})
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}

	pauseRequest(fake, "S1", requestStage("https://example.org/page", "GET", "Document", nil))
	waitForCall(t, fake, "Fetch.failRequest")
}

// TestADocumentResponseCarriesTheFence. The CSP is the layer that closes wss: and contains blob:,
// and it only exists where it is injected.
func TestADocumentResponseCarriesTheFence(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}

	paused := requestStage("https://example.org/page", "GET", "Document", nil)
	paused["responseStatusCode"] = 200
	paused["responseHeaders"] = []map[string]any{
		{"name": "Content-Type", "value": "text/html"},
		{"name": "Content-Security-Policy", "value": "script-src 'self'"},
	}
	pauseRequest(fake, "S1", paused)

	call := waitForCall(t, fake, "Fetch.continueResponse")
	var params struct {
		ResponseHeaders []fence.Header `json:"responseHeaders"`
	}
	if err := json.Unmarshal(call.Params, &params); err != nil {
		t.Fatalf("params: %v", err)
	}
	if !fence.CarriesFence(params.ResponseHeaders) {
		t.Fatalf("the document went through without the fence's CSP: %+v", params.ResponseHeaders)
	}

	// The page's own policy has to survive. Two CSP headers intersect; one that replaced the other
	// would be the fence quietly relaxing a site that was stricter than we are.
	var kept bool
	for _, header := range params.ResponseHeaders {
		if header.Value == "script-src 'self'" {
			kept = true
		}
	}
	if !kept {
		t.Errorf("the page's own CSP was dropped: %+v", params.ResponseHeaders)
	}
}

// TestASubresourceResponseIsNotInterceptedTwice. The control for the case above: if every resource
// asked for a response stage, the fence would double the cost of every page load for a check that
// cannot fire on a stylesheet.
func TestASubresourceResponseIsNotInterceptedTwice(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	pauseRequest(fake, "S1", requestStage("https://example.org/app.css", "GET", "Stylesheet", nil))

	call := waitForCall(t, fake, "Fetch.continueRequest")
	var params map[string]any
	if err := json.Unmarshal(call.Params, &params); err != nil {
		t.Fatalf("params: %v", err)
	}
	if params["interceptResponse"] == true {
		t.Error("a stylesheet asked for a response stage it has no use for")
	}
}

// TestADownloadIsRefusedAtTheResponse — spec §11, test 8.
func TestADownloadIsRefusedAtTheResponse(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}

	paused := requestStage("https://example.org/report.pdf", "GET", "Other", nil)
	paused["responseStatusCode"] = 200
	paused["responseHeaders"] = []map[string]any{
		{"name": "Content-Disposition", "value": `attachment; filename="report.pdf"`},
	}
	pauseRequest(fake, "S1", paused)

	waitForCall(t, fake, "Fetch.failRequest")
	if hasCall(fake, "Fetch.continueResponse") {
		t.Error("the download was continued as well as failed")
	}
}

// TestDownloadsAreDeniedAtTheBrowser is the other half of test 8: the mechanism that covers a
// download the interception never sees.
func TestDownloadsAreDeniedAtTheBrowser(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn, projectPolicy()); err != nil {
		t.Fatalf("connect: %v", err)
	}
	call := waitForCall(t, fake, "Browser.setDownloadBehavior")
	var params struct {
		Behavior string `json:"behavior"`
	}
	if err := json.Unmarshal(call.Params, &params); err != nil {
		t.Fatalf("params: %v", err)
	}
	if params.Behavior != "deny" {
		t.Fatalf("downloads are set to %q", params.Behavior)
	}
}

// TestAFenceWithNoPolicyRefusesToProduceADriver. Spec §6.2a applied to configuration: a fence
// enforcing a default is a fence nobody chose.
func TestAFenceWithNoPolicyRefusesToProduceADriver(t *testing.T) {
	fake, conn := dial(t)
	driver, err := Connect(context.Background(), conn, fence.Policy{})
	if !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
	if driver != nil {
		t.Fatal("a Driver was handed back with no policy")
	}
	if len(fake.Calls()) != 0 {
		// Checked, and not merely implied by the error: the policy must be rejected BEFORE the
		// browser is touched, or there is a window in which interception is on and enforcing
		// nothing.
		t.Errorf("the browser was touched before the policy was checked: %v", fake.Methods())
	}
}

func TestDownloadDenialFailingRefusesToProduceADriver(t *testing.T) {
	fake, conn := dial(t)
	fake.Handle("Browser.setDownloadBehavior", func(cdptest.Call) (any, error) {
		return nil, errors.New("'Browser.setDownloadBehavior' wasn't found")
	})
	if _, err := Connect(context.Background(), conn, projectPolicy()); !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
}

// TestAPopupIsClosedAndReported — spec §11, test 4. The popup is found by openerId, and the driver
// closes it rather than watching it: a headless popup is invisible by construction (§4.1).
func TestAPopupIsClosedAndReported(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	before := driver.refusalCount()
	fake.Emit("", "Target.attachedToTarget", map[string]any{
		"sessionId": "S2",
		"targetInfo": map[string]any{
			"targetId": "T2",
			"type":     "page",
			"openerId": "T1",
			"url":      "",
		},
		"waitingForDebugger": true,
	})

	waitForCall(t, fake, "Target.closeTarget")
	refusal := driver.refusalFor(context.Background(), session.ID, before)
	if refusal == nil {
		t.Fatal("the popup was closed without telling the session that opened it")
	}
	if refusal.Consequence != browser.ConsequenceNewTarget {
		t.Errorf("reported as %q", refusal.Consequence)
	}
}

// TestActReportsWhatTheFenceStopped is spec §6.2's second half — "o act que o causou responde ao
// agente" — and the reason Act pays for a settle window.
func TestActReportsWhatTheFenceStopped(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		// The click causes the request, exactly as a real one would.
		go pauseRequest(fake, "S1", requestStage("https://example.org/delete", "DELETE", "XHR", nil))
		return map[string]any{}, nil
	})

	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	driver.mu.Lock()
	driver.sessions[session.ID].refs["e1"] = nodeKey{backend: 42}
	driver.mu.Unlock()

	result, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e1",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused || !result.Valid() {
		t.Fatalf("got %+v, want a refusal", result)
	}
	if result.Refusal.Consequence != browser.ConsequenceMethod {
		t.Errorf("reported as %q, want the method", result.Refusal.Consequence)
	}
}

// TestAnActThatCausesNothingIsDone is the control for the test above. Without it, an Act that
// returned a refusal unconditionally would pass — and the agent would be told it may do nothing.
func TestAnActThatCausesNothingIsDone(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})

	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	driver.mu.Lock()
	driver.sessions[session.ID].refs["e1"] = nodeKey{backend: 42}
	driver.mu.Unlock()

	result, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e1",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("got %+v, want done", result)
	}
}

// TestOpenReportsTheUrlItLandedOn. Spec §5.3 decides on the requested url AND the final one, so a
// driver that echoed the request back would make that decision impossible.
func TestOpenReportsTheUrlItLandedOn(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.RequestedURL != "https://example.org/" {
		t.Errorf("requested url is %q", session.RequestedURL)
	}
	if session.FinalURL != "https://example.org/landed" {
		t.Errorf("final url is %q; the driver echoed the request instead of reading the target", session.FinalURL)
	}
}

// TestOpenReportsARefusedNavigationAsAValue. A blocked navigation is an answer, not a broken
// browser: the agent's next move is an ephemeral profile, not a retry.
func TestOpenReportsARefusedNavigationAsAValue(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	// The paused request is raised BY the navigation, not beside it on a timer. An earlier version
	// of this test emitted it from a goroutine 20ms in and passed most of the time: under -race the
	// refusal sometimes landed before Open had counted, and was invisible by construction. The
	// ordering is the thing under test, so the test has to reproduce the causality rather than
	// approximate it with a sleep.
	fake.Handle("Page.navigate", func(cdptest.Call) (any, error) {
		pauseRequest(fake, "S1", requestStage("https://evil.example.net/", "GET", "Document", nil))
		return map[string]any{"frameId": "F1", "errorText": "net::ERR_BLOCKED_BY_CLIENT"}, nil
	})
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}

	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://evil.example.net/"})
	if err != nil {
		t.Fatalf("a refused navigation must not be an error: %v", err)
	}
	if session.Refusal == nil {
		t.Fatal("the session came back with no refusal on it")
	}
	if session.Refusal.Consequence != browser.ConsequenceOffAllowlist {
		t.Errorf("reported as %q", session.Refusal.Consequence)
	}
	if session.FinalURL != "" {
		t.Errorf("a refused navigation reported a final url: %q", session.FinalURL)
	}
}

// TestANavigationThatFailsForRealIsStillAnError is the control for the test above: without it,
// mapping every errorText to a refusal would hide a browser that cannot reach the network.
func TestANavigationThatFailsForRealIsStillAnError(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	fake.Handle("Page.navigate", func(cdptest.Call) (any, error) {
		return map[string]any{"frameId": "F1", "errorText": "net::ERR_NAME_NOT_RESOLVED"}, nil
	})
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err == nil {
		t.Fatal("a browser that could not resolve a name reported success")
	}
}
