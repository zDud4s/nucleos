//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/chrome"
)

// The write rule against the pinned Chromium, which is the only place it is settled.
//
// Everything below the gate — the pure table in fence/policy_test.go, the window in
// chrome/write_test.go — is a claim about code. What a real browser does with a real form is a claim
// about Chromium: whether the submission arrives as a Document POST at all, whether the frame id on
// the paused request is the one the driver armed against, whether a form submitted from a key press
// looks any different from one submitted by a button. Each of those was a guess until it was run
// here, and this pillar has already retracted three guesses that looked exactly this reasonable.
//
// Every case asserts on `site.reached`, which is the SERVER saying what arrived. That is the
// difference between "the fence blocked it" and "the request was never made", and three abandoned
// spike results turned on it.

// writing opens the reply form in a profile that may submit to it, and hands back what is needed to
// press Send.
func writing(t *testing.T, path string) (*site, *chrome.Driver, browser.SessionID) {
	t.Helper()
	place := newSite(t)
	driver, _ := fenced(t, admittingWritable(place))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	t.Cleanup(cancel)

	session, err := driver.Open(ctx, browser.OpenRequest{URL: place.origin() + path})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal != nil {
		t.Fatalf("the page itself was refused, so this measures nothing: %+v", session.Refusal)
	}
	return place, driver, session.ID
}

// TestAFormAnActSubmitsOnAGrantedOriginLeaves.
//
// The one case where a POST goes out, and the control every other case in this file needs: without
// it they would all pass against a fence that simply refuses everything, which is the shape of gate
// that makes a security control look enforced while it is shut.
//
// Three assertions, and each one alone would pass on a broken version:
//
//   - the act is not refused, which is the permission;
//   - the POST ARRIVES at the server, which a submission that silently does nothing would fail;
//   - the answer is READ, which a submission that leaves and loses the page would fail — and losing
//     the page is the failure that looks like success, because the agent is left on a document it
//     did not ask for with no way to tell.
func TestAFormAnActSubmitsOnAGrantedOriginLeaves(t *testing.T) {
	place, driver, id := writing(t, "/write")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	result, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("the submission was refused on an origin a person granted: %+v", result.Refusal)
	}
	if !place.reached("POST /wrote", settle) {
		t.Fatal("the act reported done and no POST reached the server")
	}

	after, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot after: %v", err)
	}
	if !says(after, "Reply delivered") {
		t.Fatalf("the answer to the submission was not read: %+v", after.Elements)
	}
}

// TestTurningTheGrantOffMakesThatFormRefused.
//
// The control that gives the test above its meaning, and the only one that could not be inferred
// from it: the same page, the same button, the same act, and the single difference is that nobody
// granted this origin. If this passed, every assertion above would be measuring a fence that was
// never closed.
//
// It also asserts the refusal NAMES a way forward. An agent told "form-submission" and nothing else
// has one move available, which is to try again; an agent told the origin may be read and not
// written to, and that browser_handoff is how a person grants it, has a different one.
func TestTurningTheGrantOffMakesThatFormRefused(t *testing.T) {
	place := newSite(t)
	driver, _ := fenced(t, admitting(place))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: place.origin() + "/write"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if place.reached("POST /wrote", settle) {
		t.Fatal("a form left an origin nobody granted")
	}
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("the submission was stopped and the act did not say so: %+v", result)
	}
	if result.Refusal.Consequence != browser.ConsequenceForm {
		t.Errorf("a stopped submission came back as %q", result.Refusal.Consequence)
	}
	if !strings.Contains(result.Refusal.Detail, "browser_handoff") {
		t.Errorf("the refusal a person can fix does not say how: %q", result.Refusal.Detail)
	}
	if len(result.Writes) != 0 {
		t.Errorf("a refused submission was recorded as a write: %+v", result.Writes)
	}
}

// TestAFormThePageSubmitsForItselfDoesNotLeave.
//
// The fifth condition, in a real browser. The origin is granted, the form is on it, and the agent
// did nothing — which is exactly what an injection inside a granted origin looks like from here.
//
// It reads the page and does not act on it, deliberately: reading is the whole of what an agent does
// on a page it has been asked to look at, and if reading alone were enough to let a form out then
// the grant would be a licence rather than a window.
func TestAFormThePageSubmitsForItselfDoesNotLeave(t *testing.T) {
	place, driver, id := writing(t, "/selfwrite")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	if _, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{}); err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if place.reached("POST /wrote", settle) {
		t.Fatal("the page submitted its own form on a granted origin and the fence carried it")
	}
}

// TestAFormAimedAtAnotherOriginDoesNotLeave.
//
// The exfiltration shape, and the one no grant turns into something else. The act is real, the
// origin the agent is on is granted, and the form points somewhere else — which is precisely how a
// page would send what the agent has just read to a host of its choosing.
//
// A second local server on its own port is enough here, and it is worth saying why when the framed
// canvas test next door needed a different HOST. That test needed a separate PROCESS, which is site
// isolation and therefore a question about hosts. This one needs a separate ORIGIN, and a port is
// part of an origin — the fence sees the request whichever process made it.
func TestAFormAimedAtAnotherOriginDoesNotLeave(t *testing.T) {
	place := newSite(t)
	stranger := newSite(t)
	policy := admittingWritable(place)
	// Admitted to LOAD, so nothing but the write rule can be what stops this. Without it the refusal
	// would be off-allowlist and the test would prove the allowlist works, which is already known.
	policy.Loopback = append(policy.Loopback, stranger.origin())
	policy.Writable = append(policy.Writable, stranger.origin())
	driver, _ := fenced(t, policy)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: place.origin() + "/crosswrite?to=" + url.QueryEscape(stranger.origin()+"/wrote"),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if stranger.reached("POST /wrote", settle) {
		t.Fatal("a form on one origin submitted to another, which is the thing §6.2 exists to stop")
	}
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("the cross-origin submission was stopped and the act did not say so: %+v", result)
	}
}

// TestAScriptedPostDoesNotLeaveOnAGrantedOriginEither.
//
// The second condition. `fetch(..., {method: 'POST'})` on an origin a person granted, caused by a
// real click — every condition holds except that nothing on the page was submitted, a script made a
// request. That is the path hostile content takes without passing through a form at all, and it is
// why the grant is about forms rather than about origins.
//
// The ferry is what makes this worth measuring rather than assuming: a page's own fetch does not go
// through Chrome's network stack at all on this browser, it is carried by chrome/ferry.go, which has
// its own method rule. Two mechanisms have to agree, and only a real page exercises both.
func TestAScriptedPostDoesNotLeaveOnAGrantedOriginEither(t *testing.T) {
	place, driver, id := writing(t, "/fetchwrite")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionClick, Ref: button}); err != nil {
		t.Fatalf("act: %v", err)
	}
	if place.reached("POST /wrote", settle) {
		t.Fatal("a script's POST left a granted origin; the grant is about forms, not about origins")
	}
}

// TestOneActSendsOneForm.
//
// What makes the window a window rather than a switch. The click submits the form its button belongs
// to, and that form's own handler submits a second one behind it — which is how a page would spend
// an act it did not have to earn.
//
// The claim is a COUNT and not an identity: exactly one of the two left. Which one is up to
// Chromium's ordering of two submissions started in the same tick, and the rule never said which —
// asserting on a particular one would be asserting on the ordering, which is the kind of test that
// passes for months and then fails on a faster machine.
//
// One and not zero is half the assertion, and the more important half: a version of this fence that
// refused everything would satisfy "not two" perfectly.
func TestOneActSendsOneForm(t *testing.T) {
	place, driver, id := writing(t, "/twowrites")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	result, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}

	sent := 0
	for _, arrived := range place.arrivals(settle) {
		if strings.HasPrefix(arrived, "POST ") {
			sent++
		}
	}
	if sent != 1 {
		t.Fatalf("one act sent %d forms, and the window is one submission", sent)
	}
	if len(result.Writes) != 1 {
		t.Errorf("one act recorded %d writes: %+v", len(result.Writes), result.Writes)
	}
}

// TestEnterInABoxSendsTheMessage.
//
// The decision this makes concrete, and the one that changed while it was being written. A press
// with NO ref sends its key wherever focus happens to be and arms nothing, because the page can move
// focus between the reading and the key. A press WITH a ref focuses that element first and then
// sends the key — so the shape of every chat box and half the search boxes on the web still works,
// and it is `press` on the box's own ref.
//
// Without this the rule would be a real cost paid for a theoretical gain. With it, the cost is only
// a field the reading could not name.
func TestEnterInABoxSendsTheMessage(t *testing.T) {
	place, driver, id := writing(t, "/keywrite")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	box := findRef(t, snapshot, "Message")

	result, err := driver.Act(ctx, id, browser.Action{
		Kind: browser.ActionPress,
		Ref:  box,
		Text: "enter",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("Enter in a granted origin's box was refused: %+v", result.Refusal)
	}
	if !place.reached("POST /wrote", settle) {
		t.Fatal("Enter in the box sent nothing, so a chat cannot be answered")
	}
}

// TestAKeyWithNoRefArmsNothing.
//
// The other half of the decision above, and the reason it is a decision at all. The box is focused —
// this page's only field, and Chromium puts the caret in it — so the key lands in exactly the same
// place. What differs is that the act named nothing, and the fence refuses a submission caused by an
// act that could have been aimed by the page rather than by the agent.
func TestAKeyWithNoRefArmsNothing(t *testing.T) {
	place, driver, id := writing(t, "/keywrite")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	// Focus the box the way a person would, with a click, so the key genuinely lands in the form.
	// Without this the press would go to the body and the test would pass for the wrong reason.
	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	box := findRef(t, snapshot, "Message")
	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionClick, Ref: box}); err != nil {
		t.Fatalf("click: %v", err)
	}
	// Drain whatever the click did, so what follows is about the key alone.
	place.reached("POST /wrote", 500*time.Millisecond)

	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionPress, Text: "enter"}); err != nil {
		t.Fatalf("act: %v", err)
	}
	if place.reached("POST /wrote", settle) {
		t.Fatal("a key sent wherever focus happened to be submitted a form")
	}
}

// TestAWriteThatLeftIsWrittenDownByItsFieldNamesAndNeverItsValues.
//
// The record, measured end to end against a real form rather than against a struct somebody filled
// in. What arrives is what the núcleo files, so if the driver reported the wrong thing here nothing
// downstream would know.
//
// The password field is why this test has one. It has to be NAMED — a person reviewing what an agent
// did needs to know a form with a password in it was submitted — and its contents must not be
// anywhere, which includes the action's query string, the field the same argument is easiest to
// forget about.
func TestAWriteThatLeftIsWrittenDownByItsFieldNamesAndNeverItsValues(t *testing.T) {
	place, driver, id := writing(t, "/write")
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send reply")

	result, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionClick, Ref: button})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if !place.reached("POST /wrote", settle) {
		t.Fatal("nothing was submitted, so there is nothing for the record to be about")
	}
	if len(result.Writes) != 1 {
		t.Fatalf("a submission that left was recorded %d times: %+v", len(result.Writes), result.Writes)
	}

	wrote := result.Writes[0]
	if wrote.Method != "POST" {
		t.Errorf("recorded method %q", wrote.Method)
	}
	if !strings.HasSuffix(wrote.Action, "/wrote") {
		t.Errorf("recorded action %q, want the form's own destination", wrote.Action)
	}
	if wrote.FieldCount != 3 {
		t.Errorf("recorded %d fields, want the three the form carries: %+v", wrote.FieldCount, wrote.Fields)
	}
	named := strings.Join(wrote.Fields, ",")
	for _, want := range []string{"body", "secret", "csrf"} {
		if !strings.Contains(named, want) {
			t.Errorf("the record does not name the %q field: %v", want, wrote.Fields)
		}
	}
	// And the whole of the price this design pays, asserted rather than described.
	for _, value := range []string{"hunter2", "looks fine to me", "t0ken"} {
		if strings.Contains(named+" "+wrote.Action, value) {
			t.Errorf("a submitted VALUE reached the record: %q", value)
		}
	}
	if wrote.Ref != button || wrote.Verb != string(browser.ActionClick) {
		t.Errorf("the act that caused it is %q/%q, want %q/click", wrote.Ref, wrote.Verb, button)
	}
}
