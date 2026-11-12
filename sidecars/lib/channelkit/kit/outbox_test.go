package kit

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

func outboxFixture(t *testing.T, name string) string {
	t.Helper()
	raw, err := fs.ReadFile(fixtures.FS(), "outbox/"+name)
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func newPollClient(t *testing.T, h http.Handler) *Client {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return NewClient(Config{BaseURL: srv.URL, Channel: "tg", Token: testToken})
}

func TestPollMixedBatch(t *testing.T) {
	var mu sync.Mutex
	var gotReq, gotAuth string
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		gotReq = r.Method + " " + r.URL.RequestURI()
		gotAuth = r.Header.Get("Authorization")
		mu.Unlock()
		_, _ = w.Write([]byte(outboxFixture(t, "response-mixed.json")))
	}))
	b, err := c.Poll(context.Background(), 30)
	if err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	defer mu.Unlock()
	if gotReq != "GET /channels/tg/outbox?wait=30" {
		t.Errorf("request = %q", gotReq)
	}
	if gotAuth != "Bearer "+testToken {
		t.Errorf("auth = %q", gotAuth)
	}
	if len(b.Items) != 2 || len(b.Typing) != 1 || b.Typing[0] != "4242" {
		t.Fatalf("batch = %+v", b)
	}
	if b.Items[0].ID != 41 || b.Items[0].Rescue != nil || b.Items[0].Action.Type() != contract.ActionSend {
		t.Errorf("item 0 = %+v", b.Items[0])
	}
	if b.Items[1].ID != 44 || b.Items[1].Action.Type() != contract.ActionAnswerAction {
		t.Errorf("item 1 = %+v", b.Items[1])
	}
}

func TestPollEmptyBatch(t *testing.T) {
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(outboxFixture(t, "response-empty.json")))
	}))
	b, err := c.Poll(context.Background(), 1)
	if err != nil || len(b.Items) != 0 || len(b.Typing) != 0 {
		t.Fatalf("batch=%+v err=%v", b, err)
	}
}

func TestPollUnknownActionTypeRescuedAndNextActionSurvives(t *testing.T) {
	body := `{"actions":[` + outboxFixture(t, "unknown-action-type.json") + `,` +
		`{"id":61,"type":"send","conversation":"4242","parts":[{"type":"text","text":"ok"}]}],"ephemeral":[]}`
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(body))
	}))
	b, err := c.Poll(context.Background(), 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(b.Items) != 2 {
		t.Fatalf("items = %+v", b.Items)
	}
	r := b.Items[0]
	if r.ID != 60 || r.Action != nil || r.Rescue == nil || r.Rescue.Failed == nil ||
		r.Rescue.Failed.Retryable || r.Rescue.Failed.Reason != contract.ReasonUnsupportedAction {
		t.Errorf("rescue item = %+v", r)
	}
	if n := b.Items[1]; n.ID != 61 || n.Rescue != nil || n.Action == nil {
		t.Errorf("next item = %+v", n)
	}
}

func TestPollInvalidActionRescued(t *testing.T) {
	body := `{"actions":[` + outboxFixture(t, "send-empty-parts.json") + `],"ephemeral":[]}`
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(body))
	}))
	b, err := c.Poll(context.Background(), 1)
	if err != nil || len(b.Items) != 1 || b.Items[0].Rescue == nil ||
		b.Items[0].Rescue.Failed.Reason != contract.ReasonInvalidAction || b.Items[0].ID == 0 {
		t.Fatalf("batch=%+v err=%v", b, err)
	}
}

func TestPollIdZeroIgnoredAndLogged(t *testing.T) {
	var mu sync.Mutex
	var logged []string
	old := logf
	logf = func(format string, args ...any) {
		mu.Lock()
		logged = append(logged, fmt.Sprintf(format, args...))
		mu.Unlock()
	}
	t.Cleanup(func() { logf = old })
	body := `{"actions":[` + outboxFixture(t, "id-zero.json") + `,` +
		`{"id":7,"type":"send","conversation":"4242","parts":[{"type":"text","text":"ok"}]}],"ephemeral":[]}`
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(body))
	}))
	b, err := c.Poll(context.Background(), 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(b.Items) != 1 || b.Items[0].ID != 7 {
		t.Fatalf("items = %+v", b.Items)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(logged) != 1 {
		t.Errorf("logged = %q", logged)
	}
}

func TestPollNonJSONBodyIsRetryable(t *testing.T) {
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("<html>gateway</html>"))
	}))
	_, err := c.Poll(context.Background(), 1)
	if !errors.Is(err, ErrTransient) {
		t.Fatalf("err = %v", err)
	}
}

func TestPollMissingEphemeralIsRetryable(t *testing.T) {
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"actions":[]}`))
	}))
	if _, err := c.Poll(context.Background(), 1); !errors.Is(err, ErrTransient) {
		t.Fatalf("err = %v", err)
	}
}

func TestPollStatusClasses(t *testing.T) {
	for status, want := range map[int]error{401: ErrFatal, 403: ErrFatal, 500: ErrTransient, 429: ErrTransient} {
		c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(status)
			_, _ = w.Write([]byte(`{"error":"x"}`))
		}))
		if _, err := c.Poll(context.Background(), 1); !errors.Is(err, want) {
			t.Errorf("status %d: err = %v, want %v", status, err, want)
		}
	}
}

func TestPollContextCancelInterruptsInFlightRequest(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(started)
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { _, err := c.Poll(ctx, 30); done <- err }()
	<-started
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Errorf("err = %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Poll did not return after cancel")
	}
}

func TestPollShortDeadlineEndsSlowHandlerWithoutPanic(t *testing.T) {
	old := pollSlack
	pollSlack = 100 * time.Millisecond
	t.Cleanup(func() { pollSlack = old })
	release := make(chan struct{})
	t.Cleanup(func() { close(release) })
	c := newPollClient(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-release:
		case <-r.Context().Done():
		}
	}))
	start := time.Now()
	_, err := c.Poll(context.Background(), 0)
	if !errors.Is(err, ErrTransient) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("err = %v", err)
	}
	if d := time.Since(start); d > 3*time.Second {
		t.Errorf("took %v", d)
	}
}

func TestPollClosedServerIsRetryable(t *testing.T) {
	srv := httptest.NewServer(http.NotFoundHandler())
	url := srv.URL
	srv.Close()
	c := NewClient(Config{BaseURL: url, Channel: "tg", Token: testToken})
	_, err := c.Poll(context.Background(), 1)
	if !errors.Is(err, ErrTransient) {
		t.Fatalf("err = %v", err)
	}
	if strings.Contains(err.Error(), testToken) {
		t.Error("token leaked")
	}
}
