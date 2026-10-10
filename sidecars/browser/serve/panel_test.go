// §spec browser-com-painel

package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

func panelServer(t *testing.T, driver browser.Driver) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	panelRoutes(mux, token, driver)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

// scriptedPanel is a Fake that carries a panel: it records what is pushed, and PanelEvents sends its
// events in order, then ends the way its script says.
type scriptedPanel struct {
	*browser.Fake
	mu     sync.Mutex
	pushed map[browser.SessionID][]string
	// beforeEach runs before the event at that index is sent, so a test can hold an event back until
	// the client has proved it received the one before.
	beforeEach func(i int)
	events     []string
	end        error
	// pushErr, when set, is what PanelPush answers with.
	pushErr error
}

func (s *scriptedPanel) PanelPush(_ context.Context, id browser.SessionID, msg json.RawMessage) error {
	if s.pushErr != nil {
		return s.pushErr
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.pushed == nil {
		s.pushed = map[browser.SessionID][]string{}
	}
	s.pushed[id] = append(s.pushed[id], string(msg))
	return nil
}

func (s *scriptedPanel) PanelEvents(_ context.Context, _ browser.SessionID, sink func(json.RawMessage)) error {
	for i, event := range s.events {
		if s.beforeEach != nil {
			s.beforeEach(i)
		}
		sink(json.RawMessage(event))
	}
	return s.end
}

func (s *scriptedPanel) pushedTo(id browser.SessionID) []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.pushed[id]...)
}

// TestPanelEventsStreamsEventsThenAnEndRecord. Events arrive as N records while the channel is still
// open — the second is held back until the client has read the first — and the stream ends with exactly
// one E record carrying why.
func TestPanelEventsStreamsEventsThenAnEndRecord(t *testing.T) {
	firstRead := make(chan struct{})
	panel := &scriptedPanel{
		Fake:   &browser.Fake{FenceAttached: true},
		events: []string{`{"say":"one"}`, `{"say":"two"}`},
		end:    browser.PanelClosed{Reason: browser.PanelPersonClosed},
		beforeEach: func(i int) {
			if i == 1 {
				select {
				case <-firstRead:
				case <-time.After(5 * time.Second):
				}
			}
		},
	}
	server := panelServer(t, panel)

	response := post(t, server, "/panel/events", map[string]string{"session": "s1"}, true)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("got %d, want 200", response.StatusCode)
	}
	reader := bufio.NewReader(response.Body)
	kind, body, err := ReadRecord(reader)
	if err != nil || kind != RecordPanel || string(body) != `{"say":"one"}` {
		t.Fatalf("first record = %q %q %v", kind, body, err)
	}
	close(firstRead)
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordPanel || string(body) != `{"say":"two"}` {
		t.Fatalf("second record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordEnd {
		t.Fatalf("third record = %q %q %v, want the end record", kind, body, err)
	}
	var end struct {
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal(body, &end); err != nil || end.Reason != "person-closed" {
		t.Fatalf("end body = %q (%v), want reason person-closed", body, err)
	}
	if _, _, err := ReadRecord(reader); !errors.Is(err, io.EOF) {
		t.Fatalf("after the end record: %v, want EOF — exactly one end record", err)
	}
}

// The refusals before a stream starts are ordinary JSON errors.
func TestPanelEventsRefusals(t *testing.T) {
	t.Run("401 without the token", func(t *testing.T) {
		server := panelServer(t, &scriptedPanel{Fake: &browser.Fake{FenceAttached: true}})
		if response := post(t, server, "/panel/events", map[string]string{"session": "s1"}, false); response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
	})
	t.Run("404 no_session", func(t *testing.T) {
		panel := &scriptedPanel{Fake: &browser.Fake{FenceAttached: true}, end: browser.ErrNoSuchSession}
		server := panelServer(t, panel)
		response := post(t, server, "/panel/events", map[string]string{"session": "nope"}, true)
		if response.StatusCode != http.StatusNotFound {
			t.Fatalf("got %d, want 404", response.StatusCode)
		}
		if code := errorCode(t, response); code != "no_session" {
			t.Errorf("error = %q, want no_session", code)
		}
	})
	t.Run("501 unsupported", func(t *testing.T) {
		server := panelServer(t, halfADriver{})
		response := post(t, server, "/panel/events", map[string]string{"session": "s1"}, true)
		if response.StatusCode != http.StatusNotImplemented {
			t.Fatalf("got %d, want 501", response.StatusCode)
		}
		if code := errorCode(t, response); code != "unsupported" {
			t.Errorf("error = %q, want unsupported", code)
		}
	})
}

// TestPanelPushReachesTheDriver. The message goes to the driver untouched, under the session it names.
func TestPanelPushReachesTheDriver(t *testing.T) {
	t.Run("200 with an empty object", func(t *testing.T) {
		panel := &scriptedPanel{Fake: &browser.Fake{FenceAttached: true}}
		server := panelServer(t, panel)
		response := post(t, server, "/panel/push", map[string]any{
			"session": "s1",
			"message": map[string]string{"say": "hi"},
		}, true)
		if response.StatusCode != http.StatusOK {
			t.Fatalf("got %d, want 200", response.StatusCode)
		}
		if body := readBody(t, response); body != "{}" {
			t.Errorf("body = %q, want {}", body)
		}
		if got := panel.pushedTo("s1"); len(got) != 1 || got[0] != `{"say":"hi"}` {
			t.Fatalf("the driver was pushed %v, want the one message", got)
		}
	})
	t.Run("401 without the token", func(t *testing.T) {
		server := panelServer(t, &scriptedPanel{Fake: &browser.Fake{FenceAttached: true}})
		response := post(t, server, "/panel/push", map[string]any{"session": "s1", "message": 1}, false)
		if response.StatusCode != http.StatusUnauthorized {
			t.Fatalf("got %d, want 401", response.StatusCode)
		}
	})
	t.Run("404 no_session", func(t *testing.T) {
		panel := &scriptedPanel{Fake: &browser.Fake{FenceAttached: true}, pushErr: browser.ErrNoSuchSession}
		server := panelServer(t, panel)
		response := post(t, server, "/panel/push", map[string]any{"session": "nope", "message": 1}, true)
		if response.StatusCode != http.StatusNotFound {
			t.Fatalf("got %d, want 404", response.StatusCode)
		}
		if code := errorCode(t, response); code != "no_session" {
			t.Errorf("error = %q, want no_session", code)
		}
	})
	t.Run("501 unsupported", func(t *testing.T) {
		server := panelServer(t, halfADriver{})
		response := post(t, server, "/panel/push", map[string]any{"session": "s1", "message": 1}, true)
		if response.StatusCode != http.StatusNotImplemented {
			t.Fatalf("got %d, want 501", response.StatusCode)
		}
		if code := errorCode(t, response); code != "unsupported" {
			t.Errorf("error = %q, want unsupported", code)
		}
	})
}
