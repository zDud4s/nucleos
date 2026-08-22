package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/config"
	"nucleosbrowser/profile"
)

const token = "test-token"

// opening is a well-formed /open body: a url plus the placement the núcleo would have decided. Every
// test that is about something else uses it, so that "no placement" stays a case a test states on
// purpose rather than a state most tests happen to be in.
func opening(url string) OpenRequest {
	return OpenRequest{
		URL: url,
		Placement: browser.Placement{
			Profile: profile.Ref{Kind: profile.Ephemeral, ID: "run1"},
		},
	}
}

func testServer(t *testing.T, driver browser.Driver) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/open", authorized(token, openHandler(driver)))
	mux.HandleFunc("/snapshot", authorized(token, snapshotHandler(driver)))
	mux.HandleFunc("/act", authorized(token, actHandler(driver)))
	mux.HandleFunc("/screenshot", authorized(token, screenshotHandler(driver)))
	mux.HandleFunc("/handoff", authorized(token, handoffHandler(driver)))
	mux.HandleFunc("/close", authorized(token, closeHandler(driver)))
	wheelhouse, _ := driver.(browser.Wheelhouse)
	mux.HandleFunc("/wheel/take", authorized(token, takeWheelHandler(wheelhouse)))
	mux.HandleFunc("/wheel/return", authorized(token, returnWheelHandler(wheelhouse)))
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func post(t *testing.T, server *httptest.Server, path string, body any, auth bool) *http.Response {
	t.Helper()
	encoded, err := json.Marshal(body)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	request, err := http.NewRequest(http.MethodPost, server.URL+path, bytes.NewReader(encoded))
	if err != nil {
		t.Fatalf("request: %v", err)
	}
	if auth {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	t.Cleanup(func() { response.Body.Close() })
	return response
}

// TestEveryRouteNeedsTheToken. This process drives browsers holding the owner's logged-in sessions;
// a route that forgot the check would hand them to any local process.
func TestEveryRouteNeedsTheToken(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	for _, path := range []string{"/open", "/snapshot", "/act", "/screenshot", "/handoff", "/close"} {
		t.Run(path, func(t *testing.T) {
			response := post(t, server, path, map[string]string{"url": "https://example.org/"}, false)
			if response.StatusCode != http.StatusUnauthorized {
				t.Fatalf("without a token: got %d, want 401", response.StatusCode)
			}
		})
	}
}

// TestFenceNotAttachedIs503 pins the wire half of spec §6.2a. It must not be a 500: a crash invites
// a retry loop, and the thing being retried would be browsing without a fence.
func TestFenceNotAttachedIs503(t *testing.T) {
	server := testServer(t, &browser.Fake{}) // zero value: no fence
	response := post(t, server, "/open", opening("https://example.org/"), true)
	if response.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("got %d, want 503", response.StatusCode)
	}
}

// TestRefusalIsTwoHundred is the contract property this package exists to protect. A fenced action
// is an ANSWER: 200, with the consequence named in the body.
func TestRefusalIsTwoHundred(t *testing.T) {
	driver := &browser.Fake{
		FenceAttached: true,
		Refuse:        &browser.Refusal{Consequence: browser.ConsequenceMethod, Detail: "POST /orders"},
	}
	server := testServer(t, driver)

	var session browser.Session
	response := post(t, server, "/open", opening("https://example.org/"), true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("open: got %d", response.StatusCode)
	}
	if err := json.NewDecoder(response.Body).Decode(&session); err != nil {
		t.Fatalf("decode session: %v", err)
	}

	response = post(t, server, "/act", ActRequest{
		SessionID: string(session.ID),
		Kind:      "click",
		Ref:       "e1",
	}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("a refusal must be 200, got %d", response.StatusCode)
	}
	var result browser.ActResult
	if err := json.NewDecoder(response.Body).Decode(&result); err != nil {
		t.Fatalf("decode result: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("outcome: got %q", result.Outcome)
	}
	if result.Refusal == nil || result.Refusal.Consequence != browser.ConsequenceMethod {
		t.Fatalf("the consequence must reach the caller, got %+v", result.Refusal)
	}
}

// TestUploadCrossesTheWireWithBothOfItsArguments.
//
// **The gap this closes was live for the length of an afternoon.** The gate exercises upload against
// a real Chromium by calling `driver.Act` directly, so it never touches this package — and this
// package's request struct had no `filename` at all, while `parseKind` had never heard of the verb.
// Every test in the repository was green, and through the daemon the upload would have been rejected
// at the door.
//
// It is the failure `Asked` was added for one struct over: a field a wire shape is MISSING does not
// error. The sender fills it, the decoder finds no home for it, and the driver answers a request
// nobody made.
func TestUploadCrossesTheWireWithBothOfItsArguments(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	var session browser.Session
	response := post(t, server, "/open", opening("https://example.org/"), true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("open: got %d", response.StatusCode)
	}
	if err := json.NewDecoder(response.Body).Decode(&session); err != nil {
		t.Fatalf("decode session: %v", err)
	}

	response = post(t, server, "/act", ActRequest{
		SessionID: string(session.ID),
		Kind:      "upload",
		Ref:       "e5",
		Text:      "linha um",
		Filename:  "relatorio.txt",
	}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status = %d; upload is a kind this server has to know", response.StatusCode)
	}

	if len(driver.Actions) != 1 {
		t.Fatalf("actions = %+v, want the one upload", driver.Actions)
	}
	got := driver.Actions[0]
	if got.Kind != browser.ActionUpload {
		t.Fatalf("kind = %q", got.Kind)
	}
	if got.Filename != "relatorio.txt" {
		t.Fatalf("filename = %q: it did not survive the wire, so the driver was handed a file with "+
			"no name and would refuse it for having none", got.Filename)
	}
	if got.Text != "linha um" {
		t.Fatalf("text = %q: the contents did not survive the wire", got.Text)
	}
}

// TestTheCeilingSaysItIsTheCeiling.
//
// **Found by driving a live daemon, not by any test here.** A third session was asked for against a
// real Chromium and the caller was told `502: open failed`, while the sidecar's own log, one
// process away, said `pool: too many sessions open: 2 of 2`. The reason was written and then
// dropped: `writeDriverError`'s default arm logs the error and sends the verb plus "failed".
//
// That default is right for errors a caller cannot act on. The ceiling is not one of those — the
// answer is to close a session — and a refusal that leaves the owner guessing is the one they
// resolve by raising the limit, which is the argument `.ai/browser.yaml` already makes about
// `max_profiles`.
//
// The status is asserted as well as the body, and 409 is forced rather than chosen: the núcleo's
// `classify` maps 503 to `FenceDown`, so answering a ceiling with 503 would reach a person as
// "browsing is fenced off" — a pillar-level failure that did not happen.
func TestTheCeilingSaysItIsTheCeiling(t *testing.T) {
	driver := &browser.Fake{
		FenceAttached: true,
		OpenErr:       fmt.Errorf("%w: %d of %d", browser.ErrTooManySessions, 2, 2),
	}
	server := testServer(t, driver)

	response := post(t, server, "/open", opening("https://example.org/"), true)
	if response.StatusCode != http.StatusConflict {
		t.Fatalf("status = %d, want 409: 503 would reach the núcleo as a fence failure",
			response.StatusCode)
	}
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if !strings.Contains(string(body), "too many sessions") {
		t.Fatalf("body = %q, want it to say what the refusal was", body)
	}
	if !strings.Contains(string(body), "2 of 2") {
		t.Fatalf("body = %q: without the counts it says you are at the ceiling and never what the "+
			"ceiling is, which is the half a person can act on", body)
	}
}

// TestUnknownActionKindIsRefusedAtTheDoor. The vocabulary is closed (spec §6.2, consequence-free in
// v1); an unknown verb must not reach a driver that might interpret it generously.
func TestUnknownActionKindIsRefusedAtTheDoor(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	var session browser.Session
	response := post(t, server, "/open", opening("https://example.org/"), true)
	json.NewDecoder(response.Body).Decode(&session)

	response = post(t, server, "/act", ActRequest{
		SessionID: string(session.ID),
		Kind:      "submit",
		Ref:       "e1",
	}, true)
	if response.StatusCode != http.StatusBadRequest {
		t.Fatalf("got %d, want 400", response.StatusCode)
	}
	if len(driver.Actions) != 0 {
		t.Fatalf("the driver must never see it, got %+v", driver.Actions)
	}
}

func TestUnknownSessionIs404(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	response := post(t, server, "/snapshot", SessionRequest{SessionID: "nope"}, true)
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("got %d, want 404", response.StatusCode)
	}
}

// TestOpenWithoutAPlacementIsRefusedAtTheDoor. There is no answer to give a request that does not
// say which profile it belongs to: one guess loses the person's logins, the other hands them to a
// stranger's page (spec §5.1). So it is a 400, and the driver never sees it — the same treatment an
// unknown action kind gets, for the same reason.
func TestOpenWithoutAPlacementIsRefusedAtTheDoor(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	for _, body := range []map[string]any{
		{"url": "https://example.org/"},
		{"url": "https://example.org/", "placement": map[string]any{}},
		{"url": "https://example.org/", "placement": map[string]any{"profile": map[string]any{"id": "42"}}},
		{"url": "https://example.org/", "placement": map[string]any{
			"profile": map[string]any{"kind": "project", "id": "../chromium-1400000"}}},
	} {
		response := post(t, server, "/open", body, true)
		if response.StatusCode != http.StatusBadRequest {
			t.Errorf("%v: got %d, want 400", body, response.StatusCode)
		}
	}
	if len(driver.Opened) != 0 {
		t.Fatalf("the driver was asked to open %d of them", len(driver.Opened))
	}
}

// TestThePlacementReachesTheDriverUnchanged is the control, and the half that matters most: the
// decision the núcleo took has to arrive at the driver exactly as it was taken. A wire that dropped
// the site list would leave a project profile with an empty allowlist — which fails closed, loudly,
// and is still a bug that would look like the fence working.
func TestThePlacementReachesTheDriverUnchanged(t *testing.T) {
	driver := &browser.Fake{FenceAttached: true}
	server := testServer(t, driver)

	sent := OpenRequest{
		URL: "https://jira.example.org/browse/X-1",
		Placement: browser.Placement{
			Profile: profile.Ref{Kind: profile.Project, ID: "acme"},
			Origins: []string{"https://jira.example.org", "https://accounts.google.com"},
			// One of the two, so the assertion below can tell a list that travelled from a list that
			// was copied off the other one. A write grant dropped on the wire would be a fence that
			// refuses every submission a person granted, and nothing would report it: the agent would
			// simply be told no, correctly, about a rule nobody wrote.
			Writable: []string{"https://jira.example.org"},
		},
	}
	if response := post(t, server, "/open", sent, true); response.StatusCode != http.StatusOK {
		t.Fatalf("open: got %d", response.StatusCode)
	}
	if len(driver.Opened) != 1 {
		t.Fatalf("driver saw %d opens", len(driver.Opened))
	}
	got := driver.Opened[0]
	if got.URL != sent.URL {
		t.Errorf("url: got %q", got.URL)
	}
	if got.Placement.Profile != sent.Placement.Profile {
		t.Errorf("profile: got %+v, want %+v", got.Placement.Profile, sent.Placement.Profile)
	}
	if !slices.Equal(got.Placement.Origins, sent.Placement.Origins) {
		t.Errorf("origins: got %v, want %v", got.Placement.Origins, sent.Placement.Origins)
	}
	if !slices.Equal(got.Placement.Writable, sent.Placement.Writable) {
		t.Errorf("writable: got %v, want %v", got.Placement.Writable, sent.Placement.Writable)
	}
}

func TestGetIsNotAllowed(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	request, _ := http.NewRequest(http.MethodGet, server.URL+"/open", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("got %d, want 405", response.StatusCode)
	}
}

func TestCloseThenSnapshotIsGone(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	var session browser.Session
	response := post(t, server, "/open", opening("https://example.org/"), true)
	json.NewDecoder(response.Body).Decode(&session)

	response = post(t, server, "/close", SessionRequest{SessionID: string(session.ID)}, true)
	if response.StatusCode != http.StatusNoContent {
		t.Fatalf("close: got %d, want 204", response.StatusCode)
	}
	response = post(t, server, "/snapshot", SessionRequest{SessionID: string(session.ID)}, true)
	if response.StatusCode != http.StatusNotFound {
		t.Fatalf("snapshot after close: got %d, want 404", response.StatusCode)
	}
}

func TestServeConfigIsLoopbackOnly(t *testing.T) {
	// Serve takes whatever config.Load produced; the loopback guarantee is config's. This asserts
	// the default the two agree on, so a change to one is visible from the other.
	if config.DefaultAddr != "127.0.0.1:8795" {
		t.Fatalf("default addr drifted: %q", config.DefaultAddr)
	}
}

// halfADriver implements the driver verbs and not the wheel — the shape a single chrome.Driver has, and
// the reason serve type-asserts instead of assuming.
//
// Spelled out rather than embedding browser.Fake: the Fake DOES implement Wheelhouse, and an
// embedded one would promote those methods and make this test assert nothing.
type halfADriver struct{}

func (halfADriver) Name() string { return "half" }
func (halfADriver) Open(context.Context, browser.OpenRequest) (browser.Session, error) {
	return browser.Session{}, browser.ErrUnsupported
}
func (halfADriver) Snapshot(context.Context, browser.SessionID, browser.SnapshotRequest) (browser.Snapshot, error) {
	return browser.Snapshot{}, browser.ErrUnsupported
}
func (halfADriver) Act(context.Context, browser.SessionID, browser.Action) (browser.ActResult, error) {
	return browser.ActResult{}, browser.ErrUnsupported
}
func (halfADriver) Screenshot(context.Context, browser.SessionID) ([]byte, error) {
	return nil, browser.ErrUnsupported
}
func (halfADriver) Look(context.Context, browser.SessionID) (browser.LookResult, error) {
	return browser.LookResult{}, browser.ErrUnsupported
}
func (halfADriver) Handoff(context.Context, browser.SessionID, string) (browser.HandoffTicket, error) {
	return browser.HandoffTicket{}, browser.ErrUnsupported
}
func (halfADriver) Close(context.Context, browser.SessionID) error { return browser.ErrUnsupported }

// TestTheWheelRoutesRefuseADriverThatCannotSwapProcesses.
//
// 501 and not 500: nothing failed. Handing the wheel over means closing one browser and starting
// another over the same profile (spec §4.2), and a driver that is one browser cannot do it. Saying
// so is better than a handover that appears to work and leaves the person looking at nothing.
func TestTheWheelRoutesRefuseADriverThatCannotSwapProcesses(t *testing.T) {
	server := testServer(t, halfADriver{})

	response := post(t, server, "/wheel/take", TakeWheelRequest{
		URL:       "https://jira.example.org/",
		Placement: browser.Placement{Profile: profile.Ref{Kind: profile.Project, ID: "acme"}},
	}, true)
	defer response.Body.Close()
	if response.StatusCode != http.StatusNotImplemented {
		t.Fatalf("status = %d, want 501", response.StatusCode)
	}
}

// The wheel goes over the wire with the núcleo's placement on it, and comes back with the chain.
func TestTheWheelCrossesTheWireWithThePlacementAndReturnsTheChain(t *testing.T) {
	fake := &browser.Fake{
		FenceAttached: true,
		Chain: []string{
			"https://jira.example.org/login",
			"https://accounts.google.com/o/oauth2/auth",
			"https://jira.example.org/browse/X-1",
		},
	}
	server := testServer(t, fake)

	taken := post(t, server, "/wheel/take", TakeWheelRequest{
		URL:       "https://jira.example.org/login",
		Placement: browser.Placement{Profile: profile.Ref{Kind: profile.Project, ID: "acme"}},
	}, true)
	defer taken.Body.Close()
	if taken.StatusCode != http.StatusOK {
		t.Fatalf("take: status = %d", taken.StatusCode)
	}
	var wheel browser.Wheel
	if err := json.NewDecoder(taken.Body).Decode(&wheel); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if wheel.Mode != browser.ModeHuman {
		t.Fatalf("mode = %q, want human", wheel.Mode)
	}
	if len(fake.Wheels) != 1 || fake.Wheels[0].Placement.Profile.ID != "acme" {
		t.Fatalf("the placement did not cross the wire: %+v", fake.Wheels)
	}

	returned := post(t, server, "/wheel/return", SessionRequest{SessionID: string(wheel.Session)}, true)
	defer returned.Body.Close()
	if returned.StatusCode != http.StatusOK {
		t.Fatalf("return: status = %d", returned.StatusCode)
	}
	var back browser.Returned
	if err := json.NewDecoder(returned.Body).Decode(&back); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if !slices.Equal(back.Chain, fake.Chain) {
		t.Fatalf("chain = %v, want %v", back.Chain, fake.Chain)
	}
}

// Spec §4.5, refused at the door. A handover into a throwaway asks a person to log in somewhere that
// is deleted with the run.
func TestAHandoverIntoAThrowawayIsRefusedAtTheDoor(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})

	response := post(t, server, "/wheel/take", TakeWheelRequest{
		URL:       "https://jira.example.org/",
		Placement: browser.Placement{Profile: profile.Ref{Kind: profile.Ephemeral, ID: "r7"}},
	}, true)
	defer response.Body.Close()
	if response.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want 400", response.StatusCode)
	}
}

// The wheel routes need the token like everything else. Worth its own case because they were added
// after the six verbs, and an unauthenticated one would let anything on this machine open a window
// holding the owner's cookies.
func TestTheWheelRoutesNeedTheToken(t *testing.T) {
	server := testServer(t, &browser.Fake{FenceAttached: true})
	for _, path := range []string{"/wheel/take", "/wheel/return"} {
		response := post(t, server, path, map[string]any{}, false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Errorf("%s without a token: status = %d, want 401", path, response.StatusCode)
		}
		response.Body.Close()
	}
}
