// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The prompt group. A page asks a person things — a JS dialog today — and the person's answer comes
// over HTTP, later. What these assert is that the question never holds the connection's one dispatch
// goroutine (the fence answers on it), that it is streamed to every viewer, past and future, and that
// it is answered exactly once: by the person, by the timeout, or by the end of their turn.

// promptLog is a sink that keeps the prompts it was shown, in order.
type promptLog struct {
	mu      sync.Mutex
	prompts []browser.Prompt
}

func (p *promptLog) sink(frame browser.Frame) {
	if frame.Prompt == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.prompts = append(p.prompts, *frame.Prompt)
}

func (p *promptLog) all() []browser.Prompt {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]browser.Prompt(nil), p.prompts...)
}

// find waits for a prompt the predicate accepts and returns it.
func (p *promptLog) find(t *testing.T, what string, accept func(browser.Prompt) bool) browser.Prompt {
	t.Helper()
	var found browser.Prompt
	eventually(t, what, 3*time.Second, func() bool {
		for _, prompt := range p.all() {
			if accept(prompt) {
				found = prompt
				return true
			}
		}
		return false
	})
	return found
}

// openPrompt waits for an unresolved prompt of this kind.
func (p *promptLog) openPrompt(t *testing.T, kind string) browser.Prompt {
	t.Helper()
	return p.find(t, "an unresolved "+kind+" prompt", func(prompt browser.Prompt) bool {
		return prompt.Kind == kind && !prompt.Resolved
	})
}

// resolved waits for the P that says this prompt is over.
func (p *promptLog) resolved(t *testing.T, id string) browser.Prompt {
	t.Helper()
	return p.find(t, "the resolved record of prompt "+id, func(prompt browser.Prompt) bool {
		return prompt.ID == id && prompt.Resolved
	})
}

// personWatched is a driver whose one session is the person's and is being watched.
func personWatched(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID, *promptLog) {
	t.Helper()
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	log := &promptLog{}
	startWatch(t, driver, id, log.sink)
	return fake, driver, id, log
}

func askDialog(fake *cdptest.Browser, on, kind, message, defaultPrompt string) {
	fake.Emit(on, "Page.javascriptDialogOpening", map[string]any{
		"type":          kind,
		"message":       message,
		"defaultPrompt": defaultPrompt,
		"url":           "https://example.org/settings",
	})
}

// dialogCalls is every Page.handleJavaScriptDialog the fake received, decoded.
func dialogCalls(t *testing.T, fake *cdptest.Browser) []map[string]any {
	t.Helper()
	var out []map[string]any
	for _, call := range callsTo(fake, "Page.handleJavaScriptDialog") {
		out = append(out, paramsMap(t, call))
	}
	return out
}

func answerJSON(t *testing.T, value any) json.RawMessage {
	t.Helper()
	raw, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal answer: %v", err)
	}
	return raw
}

// TestADialogWhileThePersonDrivesBecomesAPromptWithoutBlockingDispatch. The dialog is raised and no
// answer is given: the page waits for the person, and the connection's dispatch goroutine does not.
// The proof is the request that comes after it: the fence answers it while the dialog is open.
func TestADialogWhileThePersonDrivesBecomesAPromptWithoutBlockingDispatch(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	on := string(cdpOf(driver, id))

	askDialog(fake, on, "confirm", "Delete everything?", "")
	pauseRequest(fake, on, requestStage("https://idp.example.net/login", "GET", "Document", nil))

	prompt := log.openPrompt(t, "dialog")
	if prompt.ID == "" {
		t.Error("the prompt has no id: the answer could not name it")
	}
	if prompt.DialogType != "confirm" || prompt.Message != "Delete everything?" {
		t.Errorf("the prompt = %+v, want the confirm and its question", prompt)
	}
	waitForCall(t, fake, "Fetch.continueRequest")
	if calls := dialogCalls(t, fake); len(calls) != 0 {
		t.Errorf("the driver answered the dialog itself while the person drives: %v", calls)
	}
}

// TestAnsweringADialogPromptAnswersThePage. The person's answer is the page's: accept and the text
// they typed go to Page.handleJavaScriptDialog on the page that asked, and the prompt is closed with a
// resolved record. A second answer to the same prompt finds nothing.
func TestAnsweringADialogPromptAnswersThePage(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	on := string(cdpOf(driver, id))

	askDialog(fake, on, "prompt", "Your name?", "anon")
	prompt := log.openPrompt(t, "dialog")
	if prompt.DefaultPrompt != "anon" {
		t.Errorf("the default text was lost: %+v", prompt)
	}

	err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"accept": true, "text": "Ana"}))
	if err != nil {
		t.Fatalf("Answer: %v", err)
	}

	call := waitForCall(t, fake, "Page.handleJavaScriptDialog")
	if call.Session != on {
		t.Errorf("answered session %q, want the page that asked, %q", call.Session, on)
	}
	params := paramsMap(t, call)
	if params["accept"] != true || params["promptText"] != "Ana" {
		t.Errorf("the page was answered %v, want accept=true promptText=Ana", params)
	}
	closed := log.resolved(t, prompt.ID)
	if closed.Kind != "dialog" {
		t.Errorf("the resolved record = %+v, want it to keep the kind", closed)
	}

	again := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"accept": true}))
	if !errors.Is(again, browser.ErrNoPrompt) {
		t.Errorf("a second answer = %v, want ErrNoPrompt", again)
	}
	if got := len(dialogCalls(t, fake)); got != 1 {
		t.Errorf("the page was answered %d times, want exactly once", got)
	}
}

// TestAnUnansweredPromptIsCancelledAfterItsTimeout. Nobody answers: after the timeout the dialog is
// dismissed, so a page left frozen by a person who walked away comes back, and the prompt is closed.
func TestAnUnansweredPromptIsCancelledAfterItsTimeout(t *testing.T) {
	fake, driver, id := personSession(t)
	driver.promptTimeout = 50 * time.Millisecond
	beginPerson(t, driver, id)
	log := &promptLog{}
	startWatch(t, driver, id, log.sink)
	on := string(cdpOf(driver, id))

	askDialog(fake, on, "confirm", "Leave?", "")
	prompt := log.openPrompt(t, "dialog")

	call := waitForCall(t, fake, "Page.handleJavaScriptDialog")
	if params := paramsMap(t, call); params["accept"] != false {
		t.Errorf("a timed-out dialog was answered %v, want accept=false", params)
	}
	log.resolved(t, prompt.ID)

	late := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"accept": true}))
	if !errors.Is(late, browser.ErrNoPrompt) {
		t.Errorf("an answer after the timeout = %v, want ErrNoPrompt", late)
	}
}

// TestAnswerToAnUnknownPromptIsNoPrompt. A prompt id nobody raised is ErrNoPrompt for the person who
// holds the session, and ErrNotPerson for anybody else's — the second check comes first.
func TestAnswerToAnUnknownPromptIsNoPrompt(t *testing.T) {
	_, driver, id := personSession(t)
	body := answerJSON(t, map[string]any{"accept": true})

	if err := driver.Answer(context.Background(), id, "p1", body); !errors.Is(err, browser.ErrNotPerson) {
		t.Errorf("an answer outside a person's turn = %v, want ErrNotPerson", err)
	}
	beginPerson(t, driver, id)
	if err := driver.Answer(context.Background(), id, "p-never-raised", body); !errors.Is(err, browser.ErrNoPrompt) {
		t.Errorf("an answer to an unknown prompt = %v, want ErrNoPrompt", err)
	}
}

// TestAViewerThatJoinsLateIsShownThePendingPrompt. The dialog was raised before anybody watched; the
// viewer who arrives afterwards is handed it, because it is still waiting for an answer.
func TestAViewerThatJoinsLateIsShownThePendingPrompt(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	on := string(cdpOf(driver, id))

	askDialog(fake, on, "alert", "Heads up", "")
	// The dispatch goroutine is serial: once this request is answered, the dialog before it was handled.
	pauseRequest(fake, on, requestStage("https://idp.example.net/login", "GET", "Document", nil))
	waitForCall(t, fake, "Fetch.continueRequest")

	log := &promptLog{}
	startWatch(t, driver, id, log.sink)
	prompt := log.openPrompt(t, "dialog")
	if prompt.Message != "Heads up" || prompt.DialogType != "alert" {
		t.Errorf("the late viewer was shown %+v, want the pending alert", prompt)
	}
}

// TestEndPersonCancelsThePendingPrompts. The person's turn ends with a question still open: the page
// is answered no before the fence returns, and the viewer is told the prompt is over.
func TestEndPersonCancelsThePendingPrompts(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	on := string(cdpOf(driver, id))

	askDialog(fake, on, "confirm", "Still there?", "")
	prompt := log.openPrompt(t, "dialog")

	endPerson(t, driver, id)

	calls := callsTo(fake, "Page.handleJavaScriptDialog")
	if len(calls) != 1 {
		t.Fatalf("the pending dialog was answered %d times, want once: %v", len(calls), fake.Methods())
	}
	if params := paramsMap(t, calls[0]); params["accept"] != false {
		t.Errorf("the dialog was answered %v, want accept=false", params)
	}
	if calls[0].Session != on {
		t.Errorf("answered session %q, want %q", calls[0].Session, on)
	}
	log.resolved(t, prompt.ID)

	late := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"accept": true}))
	if !errors.Is(late, browser.ErrNotPerson) {
		t.Errorf("an answer after the turn ended = %v, want ErrNotPerson", late)
	}
}
