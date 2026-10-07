// §spec browser-volante

package chrome

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"log"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
)

// The HTTP-auth group. A server that answers 401 makes Chrome pause the request and ask the client;
// the fence asked for that from Connect, so the question is ours to answer. With nobody driving the
// answer is no, which is what a pass-through used to amount to. While a person drives it is a prompt,
// and the credentials they type are the one thing that must never leave the answer's path.

const authSecret = "s3cr3t-pw"

// authChallenge raises Fetch.authRequired for the page's main frame (F1). Fetch.enable lives on the
// browser session, so that is the session the event arrives on, as in real Chrome; the page is
// named only by the frame id.
func authChallenge(fake *cdptest.Browser, requestID string) {
	authChallengeInFrame(fake, "F1", requestID)
}

// authChallengeInFrame is authChallenge for a challenge raised by the named frame.
func authChallengeInFrame(fake *cdptest.Browser, frame, requestID string) {
	fake.Emit(string(cdp.BrowserSession), "Fetch.authRequired", map[string]any{
		"requestId":    requestID,
		"frameId":      frame,
		"resourceType": "Document",
		"request":      map[string]any{"url": "https://example.org/private", "method": "GET", "headers": map[string]any{}},
		"authChallenge": map[string]any{
			"source": "Server",
			"origin": "https://example.org",
			"scheme": "basic",
			"realm":  "staff only",
		},
	})
}

// authAnswers is every Fetch.continueWithAuth the fake received.
func authAnswers(fake *cdptest.Browser) []cdptest.Call {
	return callsTo(fake, "Fetch.continueWithAuth")
}

func authResponse(t *testing.T, call cdptest.Call) map[string]any {
	t.Helper()
	params := paramsMap(t, call)
	response, ok := params["authChallengeResponse"].(map[string]any)
	if !ok {
		t.Fatalf("continueWithAuth carries no authChallengeResponse: %v", params)
	}
	return response
}

// TestTheFenceAsksToHandleAuthFromConnect. Fetch.enable is armed with handleAuthRequests, because
// without it Chrome shows its own credential sheet and the page waits on a UI nobody sees.
func TestTheFenceAsksToHandleAuthFromConnect(t *testing.T) {
	fake, _ := personDriver(t)

	calls := callsTo(fake, "Fetch.enable")
	if len(calls) == 0 {
		t.Fatalf("Fetch.enable never happened: %v", fake.Methods())
	}
	params := paramsMap(t, calls[0])
	if params["handleAuthRequests"] != true {
		t.Errorf("Fetch.enable params = %v, want handleAuthRequests=true", params)
	}
	if _, ok := params["patterns"]; !ok {
		t.Errorf("Fetch.enable lost its patterns: %v", params)
	}
}

// TestInAgentModeAnAuthChallengeIsCancelled. Nobody is there to type a password, so the challenge is
// answered CancelAuth on the page that raised it, and no prompt is made.
func TestInAgentModeAnAuthChallengeIsCancelled(t *testing.T) {
	fake, _, _ := personSession(t)

	authChallenge(fake, "A1")

	call := waitForCall(t, fake, "Fetch.continueWithAuth")
	if call.Session != string(cdp.BrowserSession) {
		t.Errorf("answered session %q, want the browser session the challenge came on", call.Session)
	}
	params := paramsMap(t, call)
	if params["requestId"] != "A1" {
		t.Errorf("answered request %v, want A1", params["requestId"])
	}
	response := authResponse(t, call)
	if response["response"] != "CancelAuth" {
		t.Errorf("the challenge was answered %v, want CancelAuth", response)
	}
	if _, leaked := response["password"]; leaked {
		t.Errorf("a cancelled challenge carries credentials: %v", response)
	}
}

// TestAnAuthChallengeWhileThePersonDrivesBecomesAPrompt. The challenge is streamed as an auth prompt
// with its origin and realm, nothing is answered on the page's behalf, and the dispatch goroutine is
// not held: the request that comes after it is still answered.
func TestAnAuthChallengeWhileThePersonDrivesBecomesAPrompt(t *testing.T) {
	fake, driver, id, prompts := personWatched(t)
	on := string(cdpOf(driver, id))

	authChallenge(fake, "A1")
	pauseRequest(fake, on, requestStage("https://idp.example.net/login", "GET", "Document", nil))

	prompt := prompts.openPrompt(t, "auth")
	if prompt.ID == "" {
		t.Error("the prompt has no id: the answer could not name it")
	}
	if prompt.Origin != "https://example.org" || prompt.Realm != "staff only" {
		t.Errorf("the prompt = %+v, want the challenge's origin and realm", prompt)
	}
	waitForCall(t, fake, "Fetch.continueRequest")
	if calls := authAnswers(fake); len(calls) != 0 {
		t.Errorf("the driver answered the challenge itself while the person drives: %v", fake.Methods())
	}
}

// TestAnsweringAnAuthPromptProvidesTheCredentials. The person's username and password go to
// Fetch.continueWithAuth as ProvideCredentials on the page that asked, the prompt is closed, and a
// second answer finds nothing.
func TestAnsweringAnAuthPromptProvidesTheCredentials(t *testing.T) {
	fake, driver, id, prompts := personWatched(t)

	authChallenge(fake, "A7")
	prompt := prompts.openPrompt(t, "auth")

	err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
		"username": "ana", "password": authSecret,
	}))
	if err != nil {
		t.Fatalf("Answer: %v", err)
	}

	call := waitForCall(t, fake, "Fetch.continueWithAuth")
	if call.Session != string(cdp.BrowserSession) {
		t.Errorf("answered session %q, want the browser session the challenge came on", call.Session)
	}
	if params := paramsMap(t, call); params["requestId"] != "A7" {
		t.Errorf("answered request %v, want A7", params["requestId"])
	}
	response := authResponse(t, call)
	if response["response"] != "ProvideCredentials" || response["username"] != "ana" || response["password"] != authSecret {
		t.Errorf("the challenge was answered %v, want ProvideCredentials with the typed credentials", response)
	}
	closed := prompts.resolved(t, prompt.ID)
	if closed.Kind != "auth" {
		t.Errorf("the resolved record = %+v, want it to keep the kind", closed)
	}

	again := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"cancel": true}))
	if !errors.Is(again, browser.ErrNoPrompt) {
		t.Errorf("a second answer = %v, want ErrNoPrompt", again)
	}
	if got := len(authAnswers(fake)); got != 1 {
		t.Errorf("the challenge was answered %d times, want exactly once", got)
	}
}

// lockedWriter serialises writes to a buffer the logger and the test both touch.
type lockedWriter struct {
	mu  *sync.Mutex
	buf *bytes.Buffer
}

func (w lockedWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buf.Write(p)
}

// TestCredentialsAppearInNoLogAndNoRecord. The password travels from the answer to the one CDP call
// and nowhere else: not into the standard logger, not into any P record a viewer is shown, and not
// into the error of an answer that does not fit.
func TestCredentialsAppearInNoLogAndNoRecord(t *testing.T) {
	var buf bytes.Buffer
	var bufMu sync.Mutex
	previous := log.Writer()
	log.SetOutput(lockedWriter{&bufMu, &buf})
	t.Cleanup(func() { log.SetOutput(previous) })

	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	var recMu sync.Mutex
	var records [][]byte
	var seen []browser.Prompt
	startWatch(t, driver, id, func(frame browser.Frame) {
		if frame.Prompt == nil {
			return
		}
		raw, _ := json.Marshal(frame.Prompt)
		recMu.Lock()
		defer recMu.Unlock()
		records = append(records, raw)
		seen = append(seen, *frame.Prompt)
	})

	authChallenge(fake, "A1")
	var prompt browser.Prompt
	eventually(t, "an unresolved auth prompt", 3*time.Second, func() bool {
		recMu.Lock()
		defer recMu.Unlock()
		for _, one := range seen {
			if one.Kind == "auth" && !one.Resolved {
				prompt = one
				return true
			}
		}
		return false
	})

	// An answer that does not fit: the error it earns must not quote the body.
	bad := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
		"username": 7, "password": authSecret,
	}))
	if !errors.Is(bad, browser.ErrBadAnswer) {
		t.Fatalf("a malformed auth answer = %v, want ErrBadAnswer", bad)
	}
	if strings.Contains(bad.Error(), authSecret) {
		t.Errorf("the error of a malformed answer quotes the password: %v", bad)
	}

	if err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{
		"username": "ana", "password": authSecret,
	})); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	waitForCall(t, fake, "Fetch.continueWithAuth")
	eventually(t, "the resolved record", 3*time.Second, func() bool {
		recMu.Lock()
		defer recMu.Unlock()
		for _, one := range seen {
			if one.ID == prompt.ID && one.Resolved {
				return true
			}
		}
		return false
	})

	bufMu.Lock()
	logged := buf.String()
	bufMu.Unlock()
	if strings.Contains(logged, authSecret) {
		t.Errorf("the password reached the log: %q", logged)
	}
	recMu.Lock()
	defer recMu.Unlock()
	for _, raw := range records {
		if strings.Contains(string(raw), authSecret) {
			t.Errorf("the password reached a P record: %s", raw)
		}
	}
}

// TestACancelledAuthPromptCancelsTheChallenge. The person says no: CancelAuth, no credentials, the
// prompt closed. A challenge still open when their turn ends is cancelled the same way, before the
// fence returns.
func TestACancelledAuthPromptCancelsTheChallenge(t *testing.T) {
	fake, driver, id, prompts := personWatched(t)

	authChallenge(fake, "A1")
	first := prompts.openPrompt(t, "auth")
	if err := driver.Answer(context.Background(), id, first.ID, answerJSON(t, map[string]any{"cancel": true})); err != nil {
		t.Fatalf("Answer: %v", err)
	}
	call := waitForCall(t, fake, "Fetch.continueWithAuth")
	response := authResponse(t, call)
	if response["response"] != "CancelAuth" {
		t.Errorf("a cancelled prompt answered %v, want CancelAuth", response)
	}
	if _, leaked := response["username"]; leaked {
		t.Errorf("a cancelled prompt carries credentials: %v", response)
	}
	prompts.resolved(t, first.ID)

	authChallenge(fake, "A2")
	second := prompts.find(t, "the second auth prompt", func(prompt browser.Prompt) bool {
		return prompt.Kind == "auth" && !prompt.Resolved && prompt.ID != first.ID
	})
	endPerson(t, driver, id)

	calls := authAnswers(fake)
	if len(calls) != 2 {
		t.Fatalf("the challenges were answered %d times, want twice: %v", len(calls), fake.Methods())
	}
	if params := paramsMap(t, calls[1]); params["requestId"] != "A2" {
		t.Errorf("the second answer is for %v, want A2", params["requestId"])
	}
	if got := authResponse(t, calls[1]); got["response"] != "CancelAuth" {
		t.Errorf("the open challenge was answered %v at the end of the turn, want CancelAuth", got)
	}
	prompts.resolved(t, second.ID)
}

// TestAChallengeFromAnotherFrameIsCancelledAndRaisesNoPrompt. The person only answers for their own
// page: a challenge whose frame is not that page's main frame (another tab, an iframe) is cancelled as
// in agent mode, and no prompt is streamed.
func TestAChallengeFromAnotherFrameIsCancelledAndRaisesNoPrompt(t *testing.T) {
	fake, _, _, prompts := personWatched(t)

	authChallengeInFrame(fake, "OTHER", "A9")

	call := waitForCall(t, fake, "Fetch.continueWithAuth")
	if response := authResponse(t, call); response["response"] != "CancelAuth" {
		t.Errorf("a foreign frame's challenge was answered %v, want CancelAuth", response)
	}
	time.Sleep(100 * time.Millisecond)
	for _, one := range prompts.all() {
		if one.Kind == "auth" {
			t.Errorf("a foreign frame's challenge raised a prompt: %+v", one)
		}
	}
}
