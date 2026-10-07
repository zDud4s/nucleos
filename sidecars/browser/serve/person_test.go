// §spec browser-volante

package serve

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"nucleosbrowser/browser"
)

func personServer(t *testing.T, driver browser.Driver) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	personRoutes(mux, token, driver)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func openFake(t *testing.T, fake *browser.Fake) browser.SessionID {
	t.Helper()
	session, err := fake.Open(t.Context(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	return session.ID
}

func readBody(t *testing.T, response *http.Response) string {
	t.Helper()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	return strings.TrimSpace(string(body))
}

func errorCode(t *testing.T, response *http.Response) string {
	t.Helper()
	var body struct {
		Error string `json:"error"`
	}
	if err := json.Unmarshal([]byte(readBody(t, response)), &body); err != nil {
		t.Fatalf("the error body is not JSON: %v", err)
	}
	return body.Error
}

// TestPersonBeginRouteAnswersPerContract.
func TestPersonBeginRouteAnswersPerContract(t *testing.T) {
	t.Run("200 with an empty object", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true}
		server := personServer(t, fake)
		id := openFake(t, fake)
		response := post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("got %d, want 200", response.StatusCode)
		}
		if body := readBody(t, response); body != "{}" {
			t.Errorf("body = %q, want {}", body)
		}
	})
	t.Run("409 not_sole_session", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true, PersonErr: browser.ErrNotSoleSession}
		server := personServer(t, fake)
		id := openFake(t, fake)
		response := post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)
		if response.StatusCode != http.StatusConflict {
			t.Fatalf("got %d, want 409", response.StatusCode)
		}
		if code := errorCode(t, response); code != "not_sole_session" {
			t.Errorf("error = %q, want not_sole_session", code)
		}
	})
	t.Run("404 no_session", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/person/begin", map[string]string{"session": "nope"}, true)
		if response.StatusCode != http.StatusNotFound {
			t.Fatalf("got %d, want 404", response.StatusCode)
		}
		if code := errorCode(t, response); code != "no_session" {
			t.Errorf("error = %q, want no_session", code)
		}
	})
	t.Run("500 carries the message", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true, PersonErr: io.ErrUnexpectedEOF}
		server := personServer(t, fake)
		id := openFake(t, fake)
		response := post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)
		if response.StatusCode != http.StatusInternalServerError {
			t.Fatalf("got %d, want 500", response.StatusCode)
		}
		if code := errorCode(t, response); code != io.ErrUnexpectedEOF.Error() {
			t.Errorf("error = %q, want the driver's message", code)
		}
	})
	t.Run("401 without a token", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/person/begin", map[string]string{"session": "s1"}, false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
	})
}

// TestPersonEndRouteReturnsTheChainOrAnError.
func TestPersonEndRouteReturnsTheChainOrAnError(t *testing.T) {
	t.Run("200 with the chain", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true, Chain: []string{"https://a.example/", "https://b.example/"}}
		server := personServer(t, fake)
		id := openFake(t, fake)
		body := map[string]string{"session": string(id)}
		if response := post(t, server, "/person/begin", body, true); response.StatusCode != http.StatusOK {
			t.Fatalf("begin: got %d", response.StatusCode)
		}
		response := post(t, server, "/person/end", body, true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("end: got %d, want 200", response.StatusCode)
		}
		var got struct {
			Chain []string `json:"chain"`
		}
		if err := json.Unmarshal([]byte(readBody(t, response)), &got); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if len(got.Chain) != 2 || got.Chain[0] != "https://a.example/" || got.Chain[1] != "https://b.example/" {
			t.Errorf("chain = %v", got.Chain)
		}
	})
	t.Run("an empty chain is [] and never null", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true}
		server := personServer(t, fake)
		id := openFake(t, fake)
		body := map[string]string{"session": string(id)}
		post(t, server, "/person/begin", body, true)
		response := post(t, server, "/person/end", body, true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("end: got %d, want 200", response.StatusCode)
		}
		if raw := readBody(t, response); !strings.Contains(raw, `"chain":[]`) {
			t.Errorf("body = %s, want an empty array", raw)
		}
	})
	t.Run("409 not_person", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true}
		server := personServer(t, fake)
		id := openFake(t, fake)
		response := post(t, server, "/person/end", map[string]string{"session": string(id)}, true)
		if response.StatusCode != http.StatusConflict {
			t.Fatalf("got %d, want 409", response.StatusCode)
		}
		if code := errorCode(t, response); code != "not_person" {
			t.Errorf("error = %q, want not_person", code)
		}
	})
	t.Run("404 for an unknown session", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/person/end", map[string]string{"session": "nope"}, true)
		if response.StatusCode != http.StatusNotFound {
			t.Fatalf("got %d, want 404", response.StatusCode)
		}
	})
	t.Run("401 without a token", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/person/end", map[string]string{"session": "s1"}, false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
	})
}

// TestInputRouteAnswersPerContract.
func TestInputRouteAnswersPerContract(t *testing.T) {
	batch := func(id browser.SessionID) map[string]any {
		return map[string]any{
			"session": string(id),
			"events":  []map[string]any{{"kind": "text", "value": "hi"}},
		}
	}
	t.Run("200 with an empty object, and the batch reaches the driver", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true}
		server := personServer(t, fake)
		id := openFake(t, fake)
		if response := post(t, server, "/person/begin", map[string]string{"session": string(id)}, true); response.StatusCode != http.StatusOK {
			t.Fatalf("begin: got %d", response.StatusCode)
		}
		response := post(t, server, "/input", batch(id), true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("got %d, want 200", response.StatusCode)
		}
		if body := readBody(t, response); body != "{}" {
			t.Errorf("body = %q, want {}", body)
		}
		if len(fake.Inputs) != 1 || len(fake.Inputs[0]) != 1 ||
			fake.Inputs[0][0].Kind != "text" || fake.Inputs[0][0].Value != "hi" {
			t.Errorf("the driver saw %v", fake.Inputs)
		}
	})
	t.Run("400 bad_event", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true, InputErr: browser.ErrBadEvent}
		server := personServer(t, fake)
		id := openFake(t, fake)
		post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)
		response := post(t, server, "/input", batch(id), true)
		if response.StatusCode != http.StatusBadRequest {
			t.Fatalf("got %d, want 400", response.StatusCode)
		}
		if code := errorCode(t, response); code != "bad_event" {
			t.Errorf("error = %q, want bad_event", code)
		}
	})
	t.Run("409 not_person", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true}
		server := personServer(t, fake)
		id := openFake(t, fake)
		response := post(t, server, "/input", batch(id), true)
		if response.StatusCode != http.StatusConflict {
			t.Fatalf("got %d, want 409", response.StatusCode)
		}
		if code := errorCode(t, response); code != "not_person" {
			t.Errorf("error = %q, want not_person", code)
		}
	})
	t.Run("404 no_session", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/input", batch("nope"), true)
		if response.StatusCode != http.StatusNotFound {
			t.Fatalf("got %d, want 404", response.StatusCode)
		}
		if code := errorCode(t, response); code != "no_session" {
			t.Errorf("error = %q, want no_session", code)
		}
	})
	t.Run("500 carries the message", func(t *testing.T) {
		fake := &browser.Fake{FenceAttached: true, InputErr: io.ErrUnexpectedEOF}
		server := personServer(t, fake)
		id := openFake(t, fake)
		post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)
		response := post(t, server, "/input", batch(id), true)
		if response.StatusCode != http.StatusInternalServerError {
			t.Fatalf("got %d, want 500", response.StatusCode)
		}
		if code := errorCode(t, response); code != io.ErrUnexpectedEOF.Error() {
			t.Errorf("error = %q, want the driver's message", code)
		}
	})
	t.Run("401 without a token", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/input", batch("s1"), false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
	})
}

// TestInputRouteRefusesABodyOver64KiB. A person's batch is a handful of events; a body past the limit
// is refused before it is parsed, and never reaches the driver.
func TestInputRouteRefusesABodyOver64KiB(t *testing.T) {
	fake := &browser.Fake{FenceAttached: true}
	server := personServer(t, fake)
	id := openFake(t, fake)
	post(t, server, "/person/begin", map[string]string{"session": string(id)}, true)

	big := map[string]any{
		"session": string(id),
		"events":  []map[string]any{{"kind": "text", "value": strings.Repeat("a", 64*1024+1)}},
	}
	response := post(t, server, "/input", big, true)
	if response.StatusCode != http.StatusRequestEntityTooLarge {
		t.Fatalf("got %d, want 413", response.StatusCode)
	}
	if code := errorCode(t, response); code != "too_large" {
		t.Errorf("error = %q, want too_large", code)
	}
	if len(fake.Inputs) != 0 {
		t.Errorf("an oversized body still reached the driver: %v", fake.Inputs)
	}
}

// answerRecorder is a Fake that can answer a prompt: it records what it was given and fails with
// answerErr when one is set. A person's turn must be open on the session, as with a real driver.
type answerRecorder struct {
	*browser.Fake
	answerErr error

	mu      sync.Mutex
	session []browser.SessionID
	prompts []string
	answers []json.RawMessage
}

func (a *answerRecorder) Answer(_ context.Context, id browser.SessionID, prompt string, answer json.RawMessage) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.answerErr != nil {
		return a.answerErr
	}
	a.session = append(a.session, id)
	a.prompts = append(a.prompts, prompt)
	a.answers = append(a.answers, append(json.RawMessage(nil), answer...))
	return nil
}

func (a *answerRecorder) answered() int {
	a.mu.Lock()
	defer a.mu.Unlock()
	return len(a.answers)
}

// TestAnswerRouteAnswersPerContract.
func TestAnswerRouteAnswersPerContract(t *testing.T) {
	body := func(id string) map[string]any {
		return map[string]any{"session": id, "prompt": "p1", "answer": map[string]any{"accept": true, "text": "Ana"}}
	}
	t.Run("200 with an empty object, and the answer reaches the driver untouched", func(t *testing.T) {
		driver := &answerRecorder{Fake: &browser.Fake{FenceAttached: true}}
		server := personServer(t, driver)
		response := post(t, server, "/answer", body("s1"), true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("got %d, want 200", response.StatusCode)
		}
		if got := readBody(t, response); got != "{}" {
			t.Errorf("body = %q, want {}", got)
		}
		driver.mu.Lock()
		defer driver.mu.Unlock()
		if len(driver.answers) != 1 || driver.session[0] != "s1" || driver.prompts[0] != "p1" {
			t.Fatalf("the driver saw session=%v prompt=%v", driver.session, driver.prompts)
		}
		var got map[string]any
		if err := json.Unmarshal(driver.answers[0], &got); err != nil || got["accept"] != true || got["text"] != "Ana" {
			t.Errorf("the answer the driver saw = %s (%v)", driver.answers[0], err)
		}
	})
	for _, one := range []struct {
		name   string
		err    error
		status int
		code   string
	}{
		{"404 no_prompt", browser.ErrNoPrompt, http.StatusNotFound, "no_prompt"},
		{"409 not_person", browser.ErrNotPerson, http.StatusConflict, "not_person"},
		{"400 bad_answer", browser.ErrBadAnswer, http.StatusBadRequest, "bad_answer"},
		{"404 no_session", browser.ErrNoSuchSession, http.StatusNotFound, "no_session"},
		{"500 carries the message", io.ErrUnexpectedEOF, http.StatusInternalServerError, io.ErrUnexpectedEOF.Error()},
	} {
		t.Run(one.name, func(t *testing.T) {
			driver := &answerRecorder{Fake: &browser.Fake{FenceAttached: true}, answerErr: one.err}
			server := personServer(t, driver)
			response := post(t, server, "/answer", body("s1"), true)
			if response.StatusCode != one.status {
				t.Fatalf("got %d, want %d", response.StatusCode, one.status)
			}
			if code := errorCode(t, response); code != one.code {
				t.Errorf("error = %q, want %q", code, one.code)
			}
		})
	}
	t.Run("501 when the driver cannot answer", func(t *testing.T) {
		server := personServer(t, &browser.Fake{FenceAttached: true})
		response := post(t, server, "/answer", body("s1"), true)
		if response.StatusCode != http.StatusNotImplemented {
			t.Fatalf("got %d, want 501", response.StatusCode)
		}
	})
	t.Run("401 without a token", func(t *testing.T) {
		driver := &answerRecorder{Fake: &browser.Fake{FenceAttached: true}}
		server := personServer(t, driver)
		response := post(t, server, "/answer", body("s1"), false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
		if driver.answered() != 0 {
			t.Error("an unauthorised answer reached the driver")
		}
	})
	t.Run("405 for another method", func(t *testing.T) {
		server := personServer(t, &answerRecorder{Fake: &browser.Fake{FenceAttached: true}})
		request, err := http.NewRequest(http.MethodGet, server.URL+"/answer", nil)
		if err != nil {
			t.Fatal(err)
		}
		request.Header.Set("Authorization", "Bearer "+token)
		response, err := http.DefaultClient.Do(request)
		if err != nil {
			t.Fatal(err)
		}
		defer response.Body.Close()
		if response.StatusCode != http.StatusMethodNotAllowed {
			t.Fatalf("got %d, want 405", response.StatusCode)
		}
	})
}

// TestAnswerRouteTakesFourteenMiBAndRefusesMore. A file answer is up to 10 MiB in base64 and a little
// over, so the limit is 14 MiB: a body under it reaches the driver, one past it is refused before it
// is parsed and never does.
func TestAnswerRouteTakesFourteenMiBAndRefusesMore(t *testing.T) {
	driver := &answerRecorder{Fake: &browser.Fake{FenceAttached: true}}
	server := personServer(t, driver)
	sized := func(payload int) map[string]any {
		return map[string]any{"session": "s1", "prompt": "p1", "answer": map[string]any{"data": strings.Repeat("a", payload)}}
	}

	response := post(t, server, "/answer", sized(14<<20-4096), true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("a body just under 14 MiB got %d, want 200", response.StatusCode)
	}
	if driver.answered() != 1 {
		t.Fatalf("the driver saw %d answers, want 1", driver.answered())
	}

	response = post(t, server, "/answer", sized(14<<20+1), true)
	if response.StatusCode != http.StatusRequestEntityTooLarge {
		t.Fatalf("a body past 14 MiB got %d, want 413", response.StatusCode)
	}
	if code := errorCode(t, response); code != "too_large" {
		t.Errorf("error = %q, want too_large", code)
	}
	if driver.answered() != 1 {
		t.Errorf("an oversized body still reached the driver: %d answers", driver.answered())
	}
}
