package chrome

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
	"nucleosbrowser/fence"
)

// The write window, at the level where the driver and the fence meet.
//
// Everything here turns on ONE question the fence cannot answer for itself: did an act cause this
// POST, or did the page submit the form by itself? The pure half of the rule is a table in
// fence/policy_test.go; what is measured here is the half that is about time — the window opening
// before the verb, closing when the act returns, and being spent by the first submission that uses
// it.
//
// Every case emits a real Fetch.requestPaused and asserts which ANSWER the driver gave it, because
// the failure this whole file is arranged around is a request nobody answered: a paused request left
// hanging wedges the renderer, and a wedged renderer is indistinguishable from a fence that blocked
// something. That is how the original spike produced a false PASS.

// writePolicy may read example.org and submit forms to it. jira is the control that appears in the
// pure table: read in the same profile, written to in neither.
func writePolicy() fence.Policy {
	return fence.Policy{Profile: fence.Project,
		Origins:  []string{"https://example.org", "https://jira.example.com"},
		Writable: []string{"https://example.org"}}
}

// connectedUnder is `connected` with a policy of the caller's choosing. The write rule is the first
// thing in this package whose behaviour depends on which policy the driver was built with, so the
// shared helper's fixed one is not enough.
func connectedUnder(t *testing.T, policy fence.Policy) (*cdptest.Browser, *Driver) {
	t.Helper()
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn, policy)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	driver.readyWithin = 150 * time.Millisecond
	driver.idleGrace = 30 * time.Millisecond
	driver.settleWithin = 30 * time.Millisecond
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) { return reachable(), nil })
	return fake, driver
}

// onAForm makes the page answer BOTH questions a click asks: where the element is, and which form it
// would submit. They arrive on the same CDP method, so the handler tells them apart by what the
// function it is being asked to run mentions — which is what a fake for two questions on one method
// has to do, and is why the driver's two questions are worded differently enough to be told apart.
func onAForm(fake *cdptest.Browser, href string, fields ...string) {
	answer, err := json.Marshal(map[string]any{
		"href":   href,
		"fields": fields,
		"count":  len(fields),
	})
	if err != nil {
		panic(err)
	}
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		var params struct {
			FunctionDeclaration string `json:"functionDeclaration"`
		}
		_ = json.Unmarshal(call.Params, &params)
		if strings.Contains(params.FunctionDeclaration, "location.href") {
			// aimAnswer is the generic "the page returned this string" shape; its name comes from
			// the first caller rather than from anything about aiming.
			return aimAnswer(string(answer)), nil
		}
		return reachable(), nil
	})
}

// submitsOnRelease makes the page submit a form the instant the mouse comes back up, which is what a
// real submit button does and is the only way to get a paused request to arrive WHILE an act is in
// flight — which is the whole of what a window bounded by the act means.
func submitsOnRelease(fake *cdptest.Browser, urls ...string) {
	fake.Handle("Input.dispatchMouseEvent", func(call cdptest.Call) (any, error) {
		var params struct {
			Type string `json:"type"`
		}
		_ = json.Unmarshal(call.Params, &params)
		if params.Type != "mouseReleased" {
			return map[string]any{}, nil
		}
		for i, url := range urls {
			paused := requestStage(url, "POST", "Document", nil)
			paused["requestId"] = "W" + string(rune('1'+i))
			pauseRequest(fake, "S1", paused)
		}
		return map[string]any{}, nil
	})
}

// fenceAnswer says how the fence answered a paused request: "continue", "fail", or "" for never.
func fenceAnswer(t *testing.T, fake *cdptest.Browser, requestID string) string {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		for _, call := range fake.Calls() {
			var params struct {
				RequestID string `json:"requestId"`
			}
			if err := json.Unmarshal(call.Params, &params); err != nil || params.RequestID != requestID {
				continue
			}
			switch call.Method {
			case "Fetch.continueRequest":
				return "continue"
			case "Fetch.failRequest":
				return "fail"
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	return ""
}

// TestAFormSubmittedByAnActLeaves.
//
// The one case where a POST goes out, and every other test here is a way of taking one condition
// away from it. Without this the rest would pass against a fence that refuses everything, which is
// the shape of test suite that makes a security control look enforced while it is simply shut.
func TestAFormSubmittedByAnActLeaves(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	onAForm(fake, "https://example.org/thread/9", "body", "csrf")
	submitsOnRelease(fake, "https://example.org/thread/9/reply")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	if got := fenceAnswer(t, fake, "W1"); got != "continue" {
		t.Fatalf("the form submission was answered %q, want it carried", got)
	}
	if len(result.Writes) != 1 {
		t.Fatalf("the act that sent a form reported %d writes: %+v", len(result.Writes), result.Writes)
	}
}

// TestAFormThePageSubmitsOnItsOwnDoesNotLeave.
//
// The fifth condition, and the reason the grant is not the whole rule. Hostile content INSIDE a
// granted origin — a comment, an issue title, an email body — can submit that origin's forms with
// the agent doing nothing at all, and a grant checked on its own would carry every one of them.
func TestAFormThePageSubmitsOnItsOwnDoesNotLeave(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	_ = withRef(t, fake, driver)

	paused := requestStage("https://example.org/thread/9/reply", "POST", "Document", nil)
	paused["requestId"] = "W1"
	pauseRequest(fake, "S1", paused)

	if got := fenceAnswer(t, fake, "W1"); got != "fail" {
		t.Fatalf("a form the page submitted by itself was answered %q, want refused", got)
	}
}

// TestTheWindowClosesWhenTheActDoes.
//
// What makes this a window and not a switch. The same click, the same form, the same origin — and a
// submission that arrives after the act has returned finds it shut. Without this the first act on a
// granted origin would open the origin for the rest of the session, which is the blank cheque the
// fifth condition exists to refuse.
func TestTheWindowClosesWhenTheActDoes(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	onAForm(fake, "https://example.org/thread/9", "body")

	act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	// Only now, with the act finished and its window shut.
	paused := requestStage("https://example.org/thread/9/reply", "POST", "Document", nil)
	paused["requestId"] = "W9"
	pauseRequest(fake, "S1", paused)

	if got := fenceAnswer(t, fake, "W9"); got != "fail" {
		t.Fatalf("a submission after the act was answered %q, so the window outlived the act", got)
	}
}

// TestOneActSubmitsOneForm.
//
// The window is CONSUMED. A page that submits a second form on the same click — a tracking form, a
// second form the first one's handler reaches for — finds it already spent, and the two answers are
// different for the same act.
func TestOneActSubmitsOneForm(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	onAForm(fake, "https://example.org/thread/9", "body")
	submitsOnRelease(fake,
		"https://example.org/thread/9/reply",
		"https://example.org/telemetry")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	if got := fenceAnswer(t, fake, "W1"); got != "continue" {
		t.Fatalf("the first submission was answered %q, want it carried", got)
	}
	if got := fenceAnswer(t, fake, "W2"); got != "fail" {
		t.Fatalf("the second submission on one act was answered %q, want refused", got)
	}
	if len(result.Writes) != 1 {
		t.Fatalf("one act recorded %d writes; the window is one submission: %+v",
			len(result.Writes), result.Writes)
	}
}

// TestAFormToAnOriginWithNoGrantDoesNotLeave.
//
// The person's decision, and the control that gives the whole feature its meaning: the same act, on
// the same kind of form, refused because nobody granted this origin. If this passed with the grant
// removed, every test above would be measuring nothing.
func TestAFormToAnOriginWithNoGrantDoesNotLeave(t *testing.T) {
	readOnly := writePolicy()
	readOnly.Writable = nil
	fake, driver := connectedUnder(t, readOnly)
	id := withRef(t, fake, driver)
	onAForm(fake, "https://example.org/thread/9", "body")
	submitsOnRelease(fake, "https://example.org/thread/9/reply")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	if got := fenceAnswer(t, fake, "W1"); got != "fail" {
		t.Fatalf("a form to an origin with no write grant was answered %q, want refused", got)
	}
	// And it is not written down. What the record holds is what an agent DID as the person, so a
	// submission that was stopped belongs in the refusal record and nowhere else.
	if len(result.Writes) != 0 {
		t.Fatalf("a refused submission was recorded as a write: %+v", result.Writes)
	}
}

// TestARefLessPressArmsNothing.
//
// A press with no ref sends its key wherever focus happens to be, and the page can move focus
// between the reading and the key — so an agent that meant to send a search would submit whatever
// form the page had put under the cursor instead. The window only opens for an act that NAMES
// something the reading showed.
//
// The cost is smaller than it looks and is worth stating: a press WITH a ref focuses that element
// first and then sends the key, so Enter in a search box or a chat is still how a search or a
// message is sent. That case is the gate's.
func TestARefLessPressArmsNothing(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	session := opened(t, driver)
	onAForm(fake, "https://example.org/thread/9", "body")
	fake.Handle("Input.dispatchKeyEvent", func(call cdptest.Call) (any, error) {
		var params struct {
			Type string `json:"type"`
		}
		_ = json.Unmarshal(call.Params, &params)
		if params.Type == "keyUp" {
			paused := requestStage("https://example.org/search", "POST", "Document", nil)
			paused["requestId"] = "W1"
			pauseRequest(fake, "S1", paused)
		}
		return map[string]any{}, nil
	})

	act(t, driver, session.ID, browser.Action{Kind: browser.ActionPress, Text: "enter"})

	if got := fenceAnswer(t, fake, "W1"); got != "fail" {
		t.Fatalf("a key sent wherever focus was submitted a form; it was answered %q", got)
	}
}

// TestAWriteIsRecordedByItsFieldNamesAndNeverItsValues.
//
// The record is what makes supervision possible AFTERWARDS, given that the whole point of the grant
// is that it is not possible beforehand — the agent works alone inside it. So this asserts the shape
// of the row, and asserts hardest on what is NOT in it: a form carries passwords, tokens and private
// text, and keeping the values would turn the núcleo's database into a place where credentials come
// to rest for every form an agent ever fills.
func TestAWriteIsRecordedByItsFieldNamesAndNeverItsValues(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	onAForm(fake, "https://example.org/thread/9", "body", "password", "csrf")
	submitsOnRelease(fake, "https://example.org/thread/9/reply?token=s3cret")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	if len(result.Writes) != 1 {
		t.Fatalf("expected one write, got %+v", result.Writes)
	}
	wrote := result.Writes[0]
	if wrote.Origin != "https://example.org:443" {
		t.Errorf("recorded against %q, want the origin the grant is written in", wrote.Origin)
	}
	if wrote.Method != "POST" {
		t.Errorf("recorded method %q", wrote.Method)
	}
	if strings.Join(wrote.Fields, ",") != "body,password,csrf" {
		t.Errorf("recorded fields %v, want the names in document order", wrote.Fields)
	}
	if wrote.FieldCount != 3 {
		t.Errorf("recorded %d fields, want 3", wrote.FieldCount)
	}
	if wrote.Ref != "e1" || wrote.Verb != string(browser.ActionClick) {
		t.Errorf("the act that caused it is %q/%q, want e1/click", wrote.Ref, wrote.Verb)
	}
	// The name of a password field is worth having and its value is not, and the same goes for a
	// token that arrived in the action's query string — which is the field nobody would think to
	// look at, and is why the query is dropped rather than shortened.
	if strings.Contains(wrote.Action, "s3cret") || strings.Contains(wrote.Action, "?") {
		t.Errorf("the recorded action carries the query it was submitted with: %q", wrote.Action)
	}
	if wrote.Action != "https://example.org/thread/9/reply" {
		t.Errorf("recorded action %q", wrote.Action)
	}
}

// TestAFrameThatNamesASessionAnswersForIt.
//
// armedWrite has two ways of finding a window and the second is a fallback, not a second guess. A
// frame id names exactly one session, so when one matches, ITS answer is final — armed or not.
//
// Falling through would be the bug worth having a test for: session A acts on a form, session B on
// the same origin does not, and a request that provably belongs to B gets carried on A's window.
// This is white-box on purpose. Building two live sessions against the fake would measure the fake.
func TestAFrameThatNamesASessionAnswersForIt(t *testing.T) {
	driver := &Driver{sessions: map[browser.SessionID]*session{}}
	acting := &session{
		id:      "s1",
		frameID: "FRAME-A",
		mayWrite: &writeWindow{
			origin: "https://example.org:443",
			form:   browser.Write{Ref: "e1", Verb: "click"},
		},
	}
	idle := &session{id: "s2", frameID: "FRAME-B"}
	driver.sessions["s1"] = acting
	driver.sessions["s2"] = idle

	if owner, window := driver.armedWrite("FRAME-A", "https://example.org:443"); owner != acting || window == nil {
		t.Fatal("the frame of the session that acted did not find its own window")
	}
	if owner, window := driver.armedWrite("FRAME-B", "https://example.org:443"); owner != nil || window != nil {
		t.Fatal("a request from a session that did not act was carried on another session's window")
	}
	// And the fallback, which is what makes a form inside an iframe work at all: a subframe's id is
	// no session's main frame, so nothing matches by frame and the origin decides.
	if owner, _ := driver.armedWrite("A-SUBFRAME", "https://example.org:443"); owner != acting {
		t.Fatal("a form in a frame found no window, so no framed form could ever be submitted")
	}
	if owner, _ := driver.armedWrite("A-SUBFRAME", "https://jira.example.com:443"); owner != nil {
		t.Fatal("a window armed for one origin was used for a submission to another")
	}
}

// TestTwoSessionsArmedForOneOriginRefuseRatherThanGuess.
//
// The fallback's edge, and the direction it fails in. Two acts in flight on the same origin at the
// same instant is a state with no right answer, and inventing one would file a submission in the
// wrong session's record — which is worse than a refusal, because the record is the whole of what
// makes this supervisable afterwards.
func TestTwoSessionsArmedForOneOriginRefuseRatherThanGuess(t *testing.T) {
	driver := &Driver{sessions: map[browser.SessionID]*session{}}
	for _, id := range []browser.SessionID{"s1", "s2"} {
		driver.sessions[id] = &session{
			id:       id,
			frameID:  "FRAME-" + string(id),
			mayWrite: &writeWindow{origin: "https://example.org:443"},
		}
	}
	if owner, window := driver.armedWrite("A-SUBFRAME", "https://example.org:443"); owner != nil || window != nil {
		t.Fatal("two sessions armed for one origin produced an answer; there is no right one")
	}
}

// TestASpentWindowIsNotFoundAgain.
//
// The consumption is in armedWrite as well as in takeWrite, so a second submission does not even
// reach the decision with Armed filled in. Belt and braces on the property the whole rule turns on:
// one submission per act.
func TestASpentWindowIsNotFoundAgain(t *testing.T) {
	driver := &Driver{sessions: map[browser.SessionID]*session{}}
	entry := &session{id: "s1", frameID: "F", mayWrite: &writeWindow{origin: "https://example.org:443"}}
	driver.sessions["s1"] = entry

	owner, window := driver.armedWrite("F", "https://example.org:443")
	if owner == nil {
		t.Fatal("the window was not there to begin with")
	}
	driver.takeWrite(owner, window, "POST", "https://example.org/reply")

	if again, _ := driver.armedWrite("F", "https://example.org:443"); again != nil {
		t.Fatal("a spent window was handed out a second time")
	}
	if len(entry.writes) != 1 {
		t.Fatalf("the submission was recorded %d times", len(entry.writes))
	}
	// And taking it twice writes it down once, because takeWrite is the only writer and it checks.
	driver.takeWrite(owner, window, "POST", "https://example.org/reply")
	if len(entry.writes) != 1 {
		t.Fatalf("a second take recorded a submission that happened once: %+v", entry.writes)
	}
}

// TestWritesAreReportedOnceAndThenAreGone.
//
// Drained rather than accumulated, so a write reaches exactly one act. A record that stayed would be
// filed again by every act that followed, and a run that clicked ten times would show ten
// submissions where there was one.
func TestWritesAreReportedOnceAndThenAreGone(t *testing.T) {
	driver := &Driver{sessions: map[browser.SessionID]*session{}}
	entry := &session{id: "s1", writes: []browser.Write{{Origin: "https://example.org:443"}}}
	driver.sessions["s1"] = entry

	if drained := driver.drainWrites(entry); len(drained) != 1 {
		t.Fatalf("the first act was given %d writes, want 1", len(drained))
	}
	if drained := driver.drainWrites(entry); len(drained) != 0 {
		t.Fatalf("the next act was given the same write again: %+v", drained)
	}
}

// TestAnActOnSomethingThatIsNotAFormArmsNothing.
//
// The ordinary case, and the control for every test above: a click on a link, a heading, a menu
// item. The page answers the form question with nothing, and the window stays shut — so a
// submission that happens to arrive during that act is refused exactly as one arriving between acts
// would be.
func TestAnActOnSomethingThatIsNotAFormArmsNothing(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	// The default handler answers the form question with the aim answer, whose href field is empty —
	// which is what an element in no form produces once it has been through WriteOriginOf.
	submitsOnRelease(fake, "https://example.org/thread/9/reply")

	act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})

	if got := fenceAnswer(t, fake, "W1"); got != "fail" {
		t.Fatalf("a click on something that is not a form carried a submission; answered %q", got)
	}
}

// TestTheWindowIsShutEvenWhenTheVerbDeclines.
//
// The disarm is deferred, so it covers the paths where the verb never ran: an element with no size,
// an element under a banner. A window left open by a refused click would be a permission outliving
// an act that did not happen — which is the one thing this arrangement exists to make impossible.
func TestTheWindowIsShutEvenWhenTheVerbDeclines(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	id := withRef(t, fake, driver)
	// The form question answers; the aim question says a banner is on top, so the click refuses.
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		var params struct {
			FunctionDeclaration string `json:"functionDeclaration"`
		}
		_ = json.Unmarshal(call.Params, &params)
		if strings.Contains(params.FunctionDeclaration, "location.href") {
			return aimAnswer(`{"href":"https://example.org/thread/9","fields":["body"],"count":1}`), nil
		}
		return aimAnswer(`{"x":1,"y":1,"sized":true,"reached":false,"on_top":"div \"We use cookies\""}`), nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("the click was not refused, so this measures nothing: %+v", result)
	}

	entry, err := driver.lookup(id)
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}
	driver.mu.Lock()
	open := entry.mayWrite
	driver.mu.Unlock()
	if open != nil {
		t.Fatal("a click that refused left its write window open")
	}
}

// TestTheFenceAsksForAWindowExactlyWhenTheRuleWouldJudgeOne.
//
// NeedsWriteWindow lives in the fence package so the two sides cannot drift, and this is the driver
// side of that agreement: a request the rule judges arrives with the window looked up, and one it
// does not judge costs nothing. Asserted through the answers rather than through the lookup, because
// what a wrong answer here would produce is a page whose ordinary GETs stopped working.
func TestTheFenceAsksForAWindowExactlyWhenTheRuleWouldJudgeOne(t *testing.T) {
	fake, driver := connectedUnder(t, writePolicy())
	_ = withRef(t, fake, driver)

	for id, paused := range map[string]map[string]any{
		"G1": requestStage("https://example.org/page", "GET", "Document", nil),
		"G2": requestStage("https://example.org/app.js", "GET", "Script", nil),
	} {
		paused["requestId"] = id
		pauseRequest(fake, "S1", paused)
		if got := fenceAnswer(t, fake, id); got != "continue" {
			t.Errorf("an ordinary request was answered %q; the write rule reached something it does"+
				" not judge", got)
		}
	}
}
