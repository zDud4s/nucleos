// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
)

// The person-mode group. A person drives the agent's own browser: the fence is lifted while they do,
// and what these assert is that it is lifted for exactly that long — it comes back only after the
// pages have been reloaded (so nothing the person's session left running keeps running under the
// fence) and the profile has been swept of workers again.

// attachNumbered makes every created target attach as its own T<n>/S<n>. autoAttachOnCreate always
// answers T1/S1, which is right for one session and cannot express two. The scratch page the
// service-worker sweep opens at Connect takes number 1, so the first session a test opens is T2/S2.
func attachNumbered(fake *cdptest.Browser) {
	var mu sync.Mutex
	count := 0
	fake.Handle("Target.createTarget", func(cdptest.Call) (any, error) {
		mu.Lock()
		count++
		n := count
		mu.Unlock()
		target, session := fmt.Sprintf("T%d", n), fmt.Sprintf("S%d", n)
		go func() {
			time.Sleep(20 * time.Millisecond)
			fake.Emit("", "Target.attachedToTarget", map[string]any{
				"sessionId": session,
				"targetInfo": map[string]any{
					"targetId": target,
					"type":     "page",
					"url":      "",
				},
				"waitingForDebugger": true,
			})
		}()
		return map[string]any{"targetId": target}, nil
	})
	// Every page's top frame is F1, which is the frame id requestStage already uses for a request.
	fake.Handle("Page.getFrameTree", func(cdptest.Call) (any, error) {
		return map[string]any{"frameTree": map[string]any{"frame": map[string]any{"id": "F1"}}}, nil
	})
}

// personDriver is a connected driver with timings short enough for the act-based tests.
func personDriver(t *testing.T) (*cdptest.Browser, *Driver) {
	t.Helper()
	fake, conn := dial(t)
	attachNumbered(fake)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	driver.readyWithin = 150 * time.Millisecond
	driver.idleGrace = 30 * time.Millisecond
	driver.settleWithin = 30 * time.Millisecond
	return fake, driver
}

// personSession is a driver with exactly one open session, the precondition of BeginPerson.
func personSession(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, driver := personDriver(t)
	return fake, driver, opened(t, driver).ID
}

func cdpOf(driver *Driver, id browser.SessionID) cdp.SessionID {
	driver.mu.Lock()
	defer driver.mu.Unlock()
	return driver.sessions[id].cdp
}

func modeOf(driver *Driver, id browser.SessionID) browser.Mode {
	driver.mu.Lock()
	defer driver.mu.Unlock()
	return driver.sessions[id].mode
}

func beginPerson(t *testing.T, driver *Driver, id browser.SessionID) {
	t.Helper()
	if err := driver.BeginPerson(context.Background(), id); err != nil {
		t.Fatalf("BeginPerson: %v", err)
	}
}

func endPerson(t *testing.T, driver *Driver, id browser.SessionID) browser.Returned {
	t.Helper()
	returned, err := driver.EndPerson(context.Background(), id)
	if err != nil {
		t.Fatalf("EndPerson: %v", err)
	}
	return returned
}

// callsTo is the calls to one method, with their sessions and params.
func callsTo(fake *cdptest.Browser, method string) []cdptest.Call {
	var out []cdptest.Call
	for _, call := range fake.Calls() {
		if call.Method == method {
			out = append(out, call)
		}
	}
	return out
}

func paramsMap(t *testing.T, call cdptest.Call) map[string]any {
	t.Helper()
	var params map[string]any
	if len(call.Params) > 0 {
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("params of %s: %v", call.Method, err)
		}
	}
	return params
}

// lastIndexOf is the position of the last call to a method, or -1.
func lastIndexOf(fake *cdptest.Browser, method string) int {
	last := -1
	for i, name := range fake.Methods() {
		if name == method {
			last = i
		}
	}
	return last
}

// TestBeginPersonRefusesWhenTheBrowserHasASecondSession. The fence is browser-wide, so lifting it for
// one session lifts it for every other one the agent has open. Refused, and nothing was touched.
func TestBeginPersonRefusesWhenTheBrowserHasASecondSession(t *testing.T) {
	fake, driver := personDriver(t)
	first := opened(t, driver).ID
	opened(t, driver)

	err := driver.BeginPerson(context.Background(), first)
	if !errors.Is(err, browser.ErrNotSoleSession) {
		t.Fatalf("got %v, want ErrNotSoleSession", err)
	}
	if got := countCalls(fake, "Page.reload"); got != 0 {
		t.Errorf("a refused BeginPerson still reloaded %d page(s)", got)
	}
	if mode := modeOf(driver, first); mode != browser.ModeAgent {
		t.Errorf("a refused BeginPerson left the session in mode %q", mode)
	}
}

func TestBeginPersonAcceptsAnAgentSession(t *testing.T) {
	_, driver, id := personSession(t)

	beginPerson(t, driver, id)

	if mode := modeOf(driver, id); mode != browser.ModeHuman {
		t.Errorf("mode = %q, want the person's", mode)
	}
	// Idempotent: a second call while the person already holds the session is not an error.
	if err := driver.BeginPerson(context.Background(), id); err != nil {
		t.Errorf("the second BeginPerson: %v", err)
	}
}

// TestBeginPersonAcceptsASessionAlreadyHandedOff. Handoff is how the núcleo marks the wheel as asked
// for; the person then arrives through BeginPerson, so the mode it finds is already ModeHuman.
func TestBeginPersonAcceptsASessionAlreadyHandedOff(t *testing.T) {
	_, driver, id := personSession(t)
	if _, err := driver.Handoff(context.Background(), id, "a login"); err != nil {
		t.Fatalf("handoff: %v", err)
	}

	beginPerson(t, driver, id)

	if mode := modeOf(driver, id); mode != browser.ModeHuman {
		t.Errorf("mode = %q, want the person's", mode)
	}
}

// TestWhileThePersonDrivesAnOffListDocumentPassesAndIsRecorded. The login is the point: the identity
// provider is a host nobody listed, and the person goes there on purpose. The request is continued
// bare — no second pause for the response — and the host lands in the chain a grant is read from.
func TestWhileThePersonDrivesAnOffListDocumentPassesAndIsRecorded(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	on := string(cdpOf(driver, id))

	pauseRequest(fake, on, requestStage("https://idp.example.net/login", "GET", "Document", nil))
	call := waitForCall(t, fake, "Fetch.continueRequest")

	params := paramsMap(t, call)
	if _, present := params["interceptResponse"]; present {
		t.Errorf("the request asked for a response stage while the person drives: %v", params)
	}
	if params["requestId"] != "R1" {
		t.Errorf("continued the wrong request: %v", params)
	}
	if hasCall(fake, "Fetch.failRequest") {
		t.Error("an off-list document was refused while the person drives")
	}

	returned := endPerson(t, driver, id)
	if !slices.Contains(returned.Chain, "https://idp.example.net/login") {
		t.Errorf("the chain is missing the identity provider: %v", returned.Chain)
	}
}

// TestWhileThePersonDrivesNoDocumentGetsTheFenceCSP. The CSP is what stops a page talking to hosts
// the fence does not know; it is the fence, and the person's pages do not carry it.
func TestWhileThePersonDrivesNoDocumentGetsTheFenceCSP(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)

	paused := requestStage("https://example.org/page", "GET", "Document", nil)
	paused["responseStatusCode"] = 200
	paused["responseHeaders"] = []map[string]any{{"name": "Content-Type", "value": "text/html"}}
	pauseRequest(fake, string(cdpOf(driver, id)), paused)

	params := paramsMap(t, waitForCall(t, fake, "Fetch.continueResponse"))
	if _, present := params["responseHeaders"]; present {
		t.Errorf("a document carried injected headers while the person drives: %v", params)
	}
	if _, present := params["responseCode"]; present {
		t.Errorf("a document's status was overridden while the person drives: %v", params)
	}
	if params["requestId"] != "R1" {
		t.Errorf("continued the wrong response: %v", params)
	}
}

func TestBeginPersonReloadsTheTarget(t *testing.T) {
	fake, driver, id := personSession(t)
	on := string(cdpOf(driver, id))

	beginPerson(t, driver, id)

	reloads := callsTo(fake, "Page.reload")
	if len(reloads) != 1 {
		t.Fatalf("BeginPerson reloaded %d page(s), want 1: %v", len(reloads), fake.Methods())
	}
	if reloads[0].Session != on {
		t.Errorf("reloaded session %q, want the session's own page %q", reloads[0].Session, on)
	}
}

// TestEndPersonRestoresTheFence. After the person leaves, the same two requests that passed a moment
// ago are judged again: the off-list document is refused and the document response carries the CSP.
func TestEndPersonRestoresTheFence(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	endPerson(t, driver, id)
	on := string(cdpOf(driver, id))

	if mode := modeOf(driver, id); mode != browser.ModeAgent {
		t.Errorf("mode = %q after EndPerson, want the agent's", mode)
	}

	pauseRequest(fake, on, requestStage("https://evil.example.net/page", "GET", "Document", nil))
	waitForCall(t, fake, "Fetch.failRequest")

	paused := requestStage("https://example.org/page", "GET", "Document", nil)
	paused["requestId"] = "R2"
	paused["responseStatusCode"] = 200
	paused["responseHeaders"] = []map[string]any{{"name": "Content-Type", "value": "text/html"}}
	pauseRequest(fake, on, paused)
	params := paramsMap(t, waitForCall(t, fake, "Fetch.continueResponse"))
	if _, present := params["responseHeaders"]; !present {
		t.Errorf("a document response came back without the fence's CSP: %v", params)
	}
}

// TestEndPersonReloadsEveryTarget. What the person left running — a script, a timer, a half-open
// socket — must not outlive the lifting of the fence it was started under, in any page the browser
// has, and not only the one the person was looking at.
func TestEndPersonReloadsEveryTarget(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	// A popup the person's own clicks made is a second page the driver did not Open. Open itself now
	// refuses while a person drives, so the mark is lifted for this one call to stand the popup in.
	driver.mu.Lock()
	driver.personBegun = false
	driver.mu.Unlock()
	other := opened(t, driver).ID
	first, second := string(cdpOf(driver, id)), string(cdpOf(driver, other))
	before := len(callsTo(fake, "Page.reload"))

	endPerson(t, driver, id)

	reloaded := map[string]int{}
	for _, call := range callsTo(fake, "Page.reload")[before:] {
		reloaded[call.Session]++
	}
	if reloaded[first] != 1 || reloaded[second] != 1 {
		t.Errorf("EndPerson reloaded %v, want one reload on each of %s and %s", reloaded, first, second)
	}
}

// TestEndPersonClearsRefusalsRecordedWhileThePersonDrove. A refusal raised while the fence was
// nominally up but the person had the wheel is not news for the agent's next act: it was not the
// agent's doing, and reporting it would have the agent apologise for a click it never made.
func TestEndPersonClearsRefusalsRecordedWhileThePersonDrove(t *testing.T) {
	_, driver, id := personSession(t)
	beginPerson(t, driver, id)
	driver.recordSessionRefusal(id, browser.ConsequenceNewTarget, "a popup while the person drove")

	endPerson(t, driver, id)

	driver.mu.Lock()
	since := driver.sessions[id].reportedUpTo
	driver.mu.Unlock()
	if found, _ := driver.newRefusal(id, since); found != nil {
		t.Fatalf("the agent would be told about %+v, which happened before it had the wheel back", *found)
	}
}

// TestEndPersonSweepsServiceWorkersAgain. A worker the person's pages registered is code from a host
// nobody listed, and the agent's fence only refuses NEW ones. So the profile is swept again, after
// the reloads and before the fence is restored.
func TestEndPersonSweepsServiceWorkersAgain(t *testing.T) {
	fake, conn := dial(t)
	attachNumbered(fake)
	var register atomic.Bool
	fake.Handle("ServiceWorker.enable", func(cdptest.Call) (any, error) {
		if register.Load() {
			go fake.Emit("", "ServiceWorker.workerRegistrationUpdated", map[string]any{
				"registrations": []map[string]any{
					{"registrationId": "9", "scopeURL": "https://idp.example.net/app/", "isDeleted": false},
				},
			})
		}
		return map[string]any{}, nil
	})
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	id := opened(t, driver).ID
	beginPerson(t, driver, id)
	sweepsBefore := countCalls(fake, "ServiceWorker.enable")
	register.Store(true)

	endPerson(t, driver, id)

	if got := countCalls(fake, "ServiceWorker.enable") - sweepsBefore; got != 1 {
		t.Fatalf("EndPerson swept %d time(s), want 1", got)
	}
	unregistered := callsTo(fake, "ServiceWorker.unregister")
	if len(unregistered) != 1 {
		t.Fatalf("unregistered %d scope(s), want the one the person's page registered", len(unregistered))
	}
	if lastIndexOf(fake, "Page.reload") > lastIndexOf(fake, "ServiceWorker.enable") {
		t.Errorf("the sweep ran before the reload: %v", fake.Methods())
	}
}

// TestAgentVerbsAreRefusedWhileThePersonDrives. Act answers with the person-driving consequence, not
// the wheel-requested one a plain handoff gives; the reading verbs return the error every other
// agent verb gives a person's session.
func TestAgentVerbsAreRefusedWhileThePersonDrives(t *testing.T) {
	_, driver, id := personSession(t)
	beginPerson(t, driver, id)
	ctx := context.Background()

	result, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionPress, Text: "Enter"})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused || result.Refusal == nil ||
		result.Refusal.Consequence != browser.ConsequencePersonDriving {
		t.Errorf("act = %+v, want a refusal with the person-driving consequence", result)
	}
	if _, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("snapshot: %v", err)
	}
	if _, err := driver.Look(ctx, id); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("look: %v", err)
	}
	if _, err := driver.Screenshot(ctx, id); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("screenshot: %v", err)
	}
}

// TestAVerbInFlightFinishesBeforeBeginPersonSwaps. An act that has begun is not cut off, and the
// swap does not happen under it: BeginPerson waits for the verb, and its reload is the first thing
// after the verb's last call.
func TestAVerbInFlightFinishesBeforeBeginPersonSwaps(t *testing.T) {
	fake, driver, id := personSession(t)
	var slowed atomic.Bool
	fake.Handle("Input.dispatchKeyEvent", func(cdptest.Call) (any, error) {
		if slowed.CompareAndSwap(false, true) {
			time.Sleep(300 * time.Millisecond)
		}
		return nil, nil
	})

	acted := make(chan struct{})
	go func() {
		defer close(acted)
		_, _ = driver.Act(context.Background(), id, browser.Action{Kind: browser.ActionPress, Text: "Enter"})
	}()
	waitForCall(t, fake, "Input.dispatchKeyEvent")

	beginPerson(t, driver, id)

	select {
	case <-acted:
	default:
		t.Fatal("BeginPerson returned while the act was still in flight")
	}
	reload := fake.IndexOf("Page.reload")
	if reload < 0 {
		t.Fatalf("BeginPerson never reloaded: %v", fake.Methods())
	}
	if last := lastIndexOf(fake, "Input.dispatchKeyEvent"); reload < last {
		t.Errorf("the reload (%d) came before the act's last call (%d): %v", reload, last, fake.Methods())
	}
}

// TestPolicyReportsTheAgentFenceWhileThePersonDrives. Policy() is what a health readout shows, and
// the person's turn is not a change to the fence — it is a lease on it. A readout that said the
// fence was gone would be reporting a state nobody configured.
func TestPolicyReportsTheAgentFenceWhileThePersonDrives(t *testing.T) {
	_, driver, id := personSession(t)
	beginPerson(t, driver, id)

	got := driver.Policy()
	want := projectPolicy()
	if got.Profile != want.Profile || !slices.Equal(got.Origins, want.Origins) {
		t.Errorf("Policy() = %+v while the person drives, want %+v", got, want)
	}
}

// TestAnOpenAfterBeginPersonIsRefusedBeforeItNavigates. The sole-session check of BeginPerson and the
// session insert of Open are one critical section, so an Open that loses the race must not navigate
// with the fence already lifted: it is refused before Page.navigate reaches the browser.
func TestAnOpenAfterBeginPersonIsRefusedBeforeItNavigates(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	before := countCalls(fake, "Page.navigate")

	_, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/second"})
	if !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Fatalf("Open while a person holds the browser = %v, want ErrPersonIsDriving", err)
	}
	if got := countCalls(fake, "Page.navigate"); got != before {
		t.Errorf("Page.navigate was called %d times after BeginPerson, want none: %v", got-before, fake.Methods())
	}
}

// switchLog records every call to the person switch together with how many CDP calls the fake had
// seen at that moment, so a test can say where in the sequence the switch moved.
type switchLog struct {
	mu      sync.Mutex
	fake    *cdptest.Browser
	changes []bool
	at      []int
}

func (l *switchLog) record(on bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.changes = append(l.changes, on)
	l.at = append(l.at, len(l.fake.Methods()))
}

func (l *switchLog) snapshot() ([]bool, []int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return slices.Clone(l.changes), slices.Clone(l.at)
}

// visiblePersonDriver is a connected driver in the visible (panel) shape, its person switch recorded.
func visiblePersonDriver(t *testing.T) (*cdptest.Browser, *Driver, *switchLog) {
	t.Helper()
	fake, driver := personDriver(t)
	log := &switchLog{fake: fake}
	driver.MakeVisible(log.record)
	return fake, driver, log
}

// TestPanelBeginPersonDoesNotReloadOrInterceptTheFileChooser. In a visible window the person sees the
// native file dialog and the page they were looking at is theirs as it stands: both the interception
// and the reload belong to the shell-seat path only.
func TestPanelBeginPersonDoesNotReloadOrInterceptTheFileChooser(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID

	beginPerson(t, driver, id)

	if got := countCalls(fake, "Page.reload"); got != 0 {
		t.Errorf("a panel BeginPerson reloaded %d page(s): %v", got, fake.Methods())
	}
	if got := countCalls(fake, "Page.setInterceptFileChooserDialog"); got != 0 {
		t.Errorf("a panel BeginPerson intercepted the file chooser %d time(s): %v", got, fake.Methods())
	}
}

// TestPanelPersonSwitchIsOnOnlyBetweenBeginAndTheStartOfEndPerson. The switch comes on last, once
// BeginPerson has succeeded, and goes off in the same step the person state is cleared: before a
// single reload or the service-worker sweep, so the fence is back for everything those fetch.
func TestPanelPersonSwitchIsOnOnlyBetweenBeginAndTheStartOfEndPerson(t *testing.T) {
	fake, driver, log := visiblePersonDriver(t)
	id := opened(t, driver).ID
	if changes, _ := log.snapshot(); len(changes) != 0 {
		t.Fatalf("the switch moved before any person arrived: %v", changes)
	}

	beginPerson(t, driver, id)
	afterBegin := len(fake.Methods())
	changes, at := log.snapshot()
	if len(changes) != 1 || !changes[0] {
		t.Fatalf("after BeginPerson the switch changes are %v, want exactly [true]", changes)
	}
	if at[0] != afterBegin {
		t.Errorf("the switch came on after %d calls but BeginPerson made %d: it was not the last step", at[0], afterBegin)
	}

	endPerson(t, driver, id)
	changes, at = log.snapshot()
	if len(changes) != 2 || changes[1] {
		t.Fatalf("after EndPerson the switch changes are %v, want [true false]", changes)
	}
	reloads := callsTo(fake, "Page.reload")
	if len(reloads) == 0 {
		t.Fatalf("EndPerson reloaded nothing: %v", fake.Methods())
	}
	firstReload := slices.Index(fake.Methods(), "Page.reload")
	if at[1] > firstReload {
		t.Errorf("the switch went off after %d calls, after the first reload at %d: the reloads ran unfenced", at[1], firstReload)
	}
	if at[1] < afterBegin {
		t.Errorf("the switch went off at %d, before BeginPerson even finished at %d", at[1], afterBegin)
	}
}

// TestPanelBeginPersonThatFailsLeavesTheSwitchOff. A refused BeginPerson (here a second session) must
// not have lifted anything, for a fence lifted by a call that reported failure is lifted for nobody.
func TestPanelBeginPersonThatFailsLeavesTheSwitchOff(t *testing.T) {
	_, driver, log := visiblePersonDriver(t)
	first := opened(t, driver).ID
	opened(t, driver)

	if err := driver.BeginPerson(context.Background(), first); !errors.Is(err, browser.ErrNotSoleSession) {
		t.Fatalf("got %v, want ErrNotSoleSession", err)
	}
	if err := driver.BeginPerson(context.Background(), browser.SessionID("nobody")); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Fatalf("got %v, want ErrNoSuchSession", err)
	}

	if changes, _ := log.snapshot(); len(changes) != 0 {
		t.Errorf("a failed BeginPerson moved the person switch: %v", changes)
	}
}

// TestVisiblePersonModePopupIsResumedAndClosedByEndPerson. A popup the person's own click opened in a
// visible window is let through and resumed; EndPerson closes it before the reloads and the sweep.
func TestVisiblePersonModePopupIsResumedAndClosedByEndPerson(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID
	beginPerson(t, driver, id)

	fake.Emit(string(cdpOf(driver, id)), "Target.attachedToTarget", map[string]any{
		"sessionId": "SPOP",
		"targetInfo": map[string]any{
			"targetId": "TPOP",
			"type":     "page",
			"openerId": "T2",
			"url":      "",
		},
		"waitingForDebugger": true,
	})

	deadline := time.Now().Add(3 * time.Second)
	resumed := false
	for time.Now().Before(deadline) && !resumed {
		for _, call := range callsTo(fake, "Runtime.runIfWaitingForDebugger") {
			if call.Session == "SPOP" {
				resumed = true
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !resumed {
		t.Fatalf("the person's popup was never resumed: %v", fake.Methods())
	}
	for _, call := range fake.Calls() {
		if call.Method == "Target.closeTarget" && paramsMap(t, call)["targetId"] == "TPOP" {
			t.Fatalf("the person's popup was closed while the person still drives: %v", fake.Methods())
		}
	}

	endPerson(t, driver, id)

	var closedAt = -1
	for i, call := range fake.Calls() {
		if call.Method == "Target.closeTarget" && paramsMap(t, call)["targetId"] == "TPOP" {
			closedAt = i
		}
	}
	if closedAt < 0 {
		t.Fatalf("EndPerson never closed the popup: %v", fake.Methods())
	}
	if firstReload := slices.Index(fake.Methods(), "Page.reload"); firstReload >= 0 && closedAt > firstReload {
		t.Errorf("the popup was closed at %d, after the first reload at %d", closedAt, firstReload)
	}
}
