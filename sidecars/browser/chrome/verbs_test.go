package chrome

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// ready gives the driver a session with one ref in it, which is what every verb that names an
// element needs before it can be asked for anything.
func withRef(t *testing.T, fake *cdptest.Browser, driver *Driver) browser.SessionID {
	t.Helper()
	session := opened(t, driver)
	driver.sessions[session.ID].refs["e1"] = nodeKey{session: "S1", backend: 42}
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})
	return session.ID
}

// paramsOf returns the parameters of the nth call of a method, so a test can assert what was
// actually sent rather than that something was.
func paramsOf(t *testing.T, fake *cdptest.Browser, method string, nth int) map[string]any {
	t.Helper()
	seen := 0
	for _, call := range fake.Calls() {
		if call.Method != method {
			continue
		}
		if seen == nth {
			var params map[string]any
			if err := json.Unmarshal(call.Params, &params); err != nil {
				t.Fatalf("params of %s: %v", method, err)
			}
			return params
		}
		seen++
	}
	t.Fatalf("no call %d to %s; calls were %v", nth, method, fake.Methods())
	return nil
}

func act(t *testing.T, driver *Driver, id browser.SessionID, action browser.Action) browser.ActResult {
	t.Helper()
	result, err := driver.Act(context.Background(), id, action)
	if err != nil {
		t.Fatalf("act %s: %v", action.Kind, err)
	}
	return result
}

// TestEnterCarriesTheCharacterThatSubmitsAForm.
//
// Typing goes through Input.insertText, which is what a paste does and therefore fires no key event
// at all. So a search box that submits on Enter could not be submitted, and the agent's only
// evidence was a page that did not change — indistinguishable from a search with no results.
func TestEnterCarriesTheCharacterThatSubmitsAForm(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)

	result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Ref: "e1", Text: "Enter"})
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("pressing Enter was refused: %+v", result.Refusal)
	}

	down := paramsOf(t, fake, "Input.dispatchKeyEvent", 0)
	if down["text"] != "\r" {
		t.Errorf("Enter was sent without the character that submits a form: %+v", down)
	}
	if down["key"] != "Enter" {
		t.Errorf("wrong key: %+v", down)
	}
	if up := paramsOf(t, fake, "Input.dispatchKeyEvent", 1); up["type"] != "keyUp" {
		t.Errorf("a key that goes down and never comes up leaves the page holding it: %+v", up)
	}
}

// TestAKeyOutsideTheSetIsRefusedWithTheSet.
//
// The set is closed on purpose and carries no modifiers. Being told only "no" would leave the agent
// guessing a second name from the same information that produced the first.
func TestAKeyOutsideTheSetIsRefusedWithTheSet(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)

	result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Ref: "e1", Text: "Ctrl+S"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a key that is not in the set was accepted")
	}
	if result.Refusal.Consequence != browser.ConsequenceNotApplicable {
		t.Errorf("a key this browser does not press is not a fence decision: %q", result.Refusal.Consequence)
	}
	if !strings.Contains(result.Refusal.Detail, "Enter") {
		t.Errorf("the refusal does not say what could have been pressed: %q", result.Refusal.Detail)
	}
}

// TestSelectingSomethingThatIsNotADropdownSaysWhatItIs.
//
// An ARIA dropdown built from divs is not a SELECT, and setting a value on one does nothing. Doing
// nothing quietly is the failure: the agent would read the unchanged page as the choice it made.
func TestSelectingSomethingThatIsNotADropdownSaysWhatItIs(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return map[string]any{"result": map[string]any{"value": "not-a-select:div"}}, nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionSelect, Ref: "e1", Text: "Portugal"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("selecting on a div was reported as done")
	}
	if !strings.Contains(result.Refusal.Detail, "div") {
		t.Errorf("the refusal does not say what the element actually is: %q", result.Refusal.Detail)
	}
}

// TestAnOptionThatIsNotThereComesBackWithTheOnesThatAre.
func TestAnOptionThatIsNotThereComesBackWithTheOnesThatAre(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return map[string]any{"result": map[string]any{"value": "no-such-option:Portugal | Spain"}}, nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionSelect, Ref: "e1", Text: "Portgual"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a misspelt option was reported as chosen")
	}
	if !strings.Contains(result.Refusal.Detail, "Portugal | Spain") {
		t.Errorf("the agent was not told what it could have picked: %q", result.Refusal.Detail)
	}
}

// TestSelectingAnOptionThatIsThereIsDone.
func TestSelectingAnOptionThatIsThereIsDone(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return map[string]any{"result": map[string]any{"value": "ok"}}, nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionSelect, Ref: "e1", Text: "Portugal"})
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("choosing an option that exists was refused: %+v", result.Refusal)
	}
	params := paramsOf(t, fake, "Runtime.callFunctionOn", 0)
	arguments, _ := params["arguments"].([]any)
	if len(arguments) != 1 {
		t.Fatalf("the wanted option was not passed to the page: %+v", params)
	}
	if first, _ := arguments[0].(map[string]any); first["value"] != "Portugal" {
		t.Errorf("wrong option passed: %+v", arguments)
	}
}

// TestScrollingWithNoRefMovesThePage.
//
// Scrolling an element into view needs a ref, so it can only reach what is already in a snapshot.
// The content that loads as a list is scrolled — by definition — is not.
func TestScrollingWithNoRefMovesThePage(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionScroll})
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("scrolling the page was refused: %+v", result.Refusal)
	}
	params := paramsOf(t, fake, "Runtime.evaluate", 0)
	expression, _ := params["expression"].(string)
	if !strings.Contains(expression, "scrollBy") {
		t.Errorf("the page was not scrolled: %q", expression)
	}
}

// TestAnUnknownScrollDirectionSaysWhichOnesExist.
func TestAnUnknownScrollDirectionSaysWhichOnesExist(t *testing.T) {
	_, driver := connected(t)
	session := opened(t, driver)

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionScroll, Text: "sideways"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a direction that does not exist was accepted")
	}
	if !strings.Contains(result.Refusal.Detail, "bottom") {
		t.Errorf("the refusal does not name the directions that do: %q", result.Refusal.Detail)
	}
}

// TestGoingBackWithNothingBehindIsRefusedAndNotAttempted.
func TestGoingBackWithNothingBehindIsRefusedAndNotAttempted(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	fake.Handle("Page.getNavigationHistory", func(cdptest.Call) (any, error) {
		return map[string]any{
			"currentIndex": 0,
			"entries":      []map[string]any{{"id": 7}},
		}, nil
	})

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionBack})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("going back from the first page was reported as done")
	}
	if fake.IndexOf("Page.navigateToHistoryEntry") >= 0 {
		t.Error("it tried anyway")
	}
}

// TestGoingBackAsksForThePreviousEntry.
func TestGoingBackAsksForThePreviousEntry(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	fake.Handle("Page.getNavigationHistory", func(cdptest.Call) (any, error) {
		return map[string]any{
			"currentIndex": 1,
			"entries":      []map[string]any{{"id": 7}, {"id": 9}},
		}, nil
	})

	if result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionBack}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("going back was refused: %+v", result.Refusal)
	}
	params := paramsOf(t, fake, "Page.navigateToHistoryEntry", 0)
	if params["entryId"] != float64(7) {
		t.Errorf("it went somewhere other than the previous page: %+v", params)
	}
}

// TestAVerbThatNamesAnElementIsRefusedWithoutOne.
func TestAVerbThatNamesAnElementIsRefusedWithoutOne(t *testing.T) {
	_, driver := connected(t)
	session := opened(t, driver)

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionClick})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a click at nothing in particular was accepted")
	}
	if result.Refusal.Consequence != browser.ConsequenceNotApplicable {
		t.Errorf("a click with no ref is not a fence decision: %q", result.Refusal.Consequence)
	}
}

// TestARefFromNoSnapshotSaysThatIsWhatHappened.
//
// This used to come back as off-allowlist, which told the agent that a security decision had been
// taken about a page when what had actually happened was that the page moved. The two point in
// opposite directions: one says ask somewhere else, the other says look again.
func TestARefFromNoSnapshotSaysThatIsWhatHappened(t *testing.T) {
	_, driver := connected(t)
	session := opened(t, driver)

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionClick, Ref: "e404"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a ref no snapshot ever showed was honoured")
	}
	if result.Refusal.Consequence != browser.ConsequenceStaleRef {
		t.Errorf("a stale ref was reported as %q", result.Refusal.Consequence)
	}
}

func countOf(fake *cdptest.Browser, method string) int {
	seen := 0
	for _, call := range fake.Calls() {
		if call.Method == method {
			seen++
		}
	}
	return seen
}

// TestGoingToAFileUrlIsRefusedBeforeItIsTried.
//
// Every other verb acts on something a snapshot showed, so the only urls reachable were ones the
// page itself offered. goto takes an address from the agent, whose context is full of text a page
// put there — and `file:` reads the disk without ever passing the interception, so the check has to
// happen here or it does not happen.
func TestGoingToAFileUrlIsRefusedBeforeItIsTried(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	navigations := countOf(fake, "Page.navigate")

	result := act(t, driver, session.ID, browser.Action{
		Kind: browser.ActionGoto, Text: "file:///C:/Users/secrets.txt",
	})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a file: url was followed")
	}
	if result.Refusal.Consequence != browser.ConsequenceScheme {
		t.Errorf("refused for the wrong reason: %q", result.Refusal.Consequence)
	}
	if countOf(fake, "Page.navigate") != navigations {
		t.Error("it navigated anyway; the check has to come before the call, not after it")
	}
}

// TestARelativeUrlResolvesAgainstThePage.
//
// Because that is the form an address takes in the words an agent is reading — "see /docs/setup".
// Making the agent reassemble the origin by hand is the operation most likely to be got wrong in
// the direction of somebody else's host.
func TestARelativeUrlResolvesAgainstThePage(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	if result := act(t, driver, session.ID, browser.Action{
		Kind: browser.ActionGoto, Text: "/docs/setup",
	}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("a relative url was refused: %+v", result.Refusal)
	}

	last := paramsOf(t, fake, "Page.navigate", countOf(fake, "Page.navigate")-1)
	if last["url"] != "https://example.org/docs/setup" {
		t.Errorf("it did not resolve against the page it was on: %+v", last)
	}
}

// TestGoingNowhereIsRefused.
func TestGoingNowhereIsRefused(t *testing.T) {
	_, driver := connected(t)
	session := opened(t, driver)

	result := act(t, driver, session.ID, browser.Action{Kind: browser.ActionGoto})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("goto with no url was accepted")
	}
}
