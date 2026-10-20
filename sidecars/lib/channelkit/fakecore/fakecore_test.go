package fakecore_test

import (
	"bytes"
	"context"
	"io"
	"io/fs"
	"net/http"
	"strings"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/fakecore"
)

func newCore(t *testing.T) *fakecore.Core {
	t.Helper()
	return fakecore.New(t, clock.NewFake(time.Date(2026, 10, 19, 10, 0, 0, 0, time.UTC)))
}

func fixture(t *testing.T, name string) []byte {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), name)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

// call performs one request with the core's token (override with auth != "").
func call(t *testing.T, c *fakecore.Core, method, path, ctype string, body []byte) (int, string) {
	t.Helper()
	status, out, err := try(c, method, path, ctype, body)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	return status, out
}

func try(c *fakecore.Core, method, path, ctype string, body []byte) (int, string, error) {
	req, err := http.NewRequest(method, c.URL()+path, bytes.NewReader(body))
	if err != nil {
		return 0, "", err
	}
	req.Header.Set("Authorization", "Bearer "+c.Token())
	if ctype != "" {
		req.Header.Set("Content-Type", ctype)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, string(b), nil
}

func evPath(c *fakecore.Core) string { return "/channels/" + c.Channel() + "/events" }

func TestStrictValidation(t *testing.T) {
	c := newCore(t)
	status, body := call(t, c, "POST", evPath(c), "application/json", []byte(`{"event_id":"x","bogus":1}`))
	if status != 400 || !strings.Contains(body, `"error"`) {
		t.Fatalf("garbage event: got %d %s, want 400", status, body)
	}
	status, _ = call(t, c, "PUT", "/channels/"+c.Channel()+"/hello", "application/json", []byte(`{"kind":"telegram"}`))
	if status != 400 {
		t.Fatalf("garbage hello: got %d, want 400", status)
	}
	if n := len(c.Events()); n != 0 {
		t.Fatalf("garbage was recorded: %d events", n)
	}
	if got := c.InvalidEvents(); len(got) != 1 {
		t.Fatalf("InvalidEvents = %v, want 1 entry", got)
	}
}

func TestHelloStoresAndChecksVersion(t *testing.T) {
	c := newCore(t)
	var hooked []contract.Hello
	c.OnHello(func(h contract.Hello) { hooked = append(hooked, h) })
	path := "/channels/" + c.Channel() + "/hello"

	status, body := call(t, c, "PUT", path, "application/json", fixture(t, "hello/telegram-full.json"))
	if status != 200 || !strings.Contains(body, contract.Version) {
		t.Fatalf("hello: got %d %s", status, body)
	}
	h, n := c.Hello()
	if n != 1 || h.Kind != "telegram" || len(hooked) != 1 {
		t.Fatalf("hello not stored: n=%d kind=%q hooked=%d", n, h.Kind, len(hooked))
	}

	status, body = call(t, c, "PUT", path, "application/json", fixture(t, "hello/compat-future-major.json"))
	if status != 409 || !strings.Contains(body, `"contract_version"`) || !strings.Contains(body, "adapter speaks 2.0.0") {
		t.Fatalf("future major: got %d %s, want 409 contract_version", status, body)
	}
	if _, n := c.Hello(); n != 1 || len(hooked) != 1 {
		t.Fatalf("incompatible hello was stored or hooked")
	}
}

func TestDedupeDuplicate(t *testing.T) {
	c := newCore(t)
	ev := fixture(t, "events/message-private-text.json")
	status, body := call(t, c, "POST", evPath(c), "application/json", ev)
	if status != 202 || !strings.Contains(body, `"accepted":true`) {
		t.Fatalf("first: got %d %s", status, body)
	}
	status, body = call(t, c, "POST", evPath(c), "application/json", ev)
	if status != 200 || !strings.Contains(body, `"duplicate":true`) {
		t.Fatalf("second: got %d %s", status, body)
	}
	if n := len(c.Events()); n != 1 {
		t.Fatalf("Events() = %d, want 1", n)
	}
}

func TestMediaTooLarge413(t *testing.T) {
	c := newCore(t)
	c.SetMediaLimit(8)
	path := "/channels/" + c.Channel() + "/media"

	status, body := call(t, c, "POST", path, "audio/ogg", []byte("12345678"))
	if status != 200 || !strings.Contains(body, `"media_id"`) {
		t.Fatalf("at the limit: got %d %s", status, body)
	}
	status, body = call(t, c, "POST", path, "audio/ogg", []byte("123456789"))
	if status != 413 {
		t.Fatalf("over the limit: got %d %s, want 413", status, body)
	}
	if strings.TrimSpace(body) != strings.TrimSpace(string(fixture(t, "errors/media-too-large.json"))) {
		t.Fatalf("413 body = %s, want the media-too-large fixture", body)
	}
}

func TestMediaRoundTrip(t *testing.T) {
	c := newCore(t)
	path := "/channels/" + c.Channel() + "/media"
	_, body := call(t, c, "POST", path, "image/png", []byte("PNG!"))
	id := strings.Split(body, `"`)[3]
	status, got := call(t, c, "GET", path+"/"+id, "", nil)
	if status != 200 || got != "PNG!" {
		t.Fatalf("download: got %d %q", status, got)
	}
	if _, mime, ok := c.Media(id); !ok || mime != "image/png" {
		t.Fatalf("Media(%s) mime=%q ok=%v", id, mime, ok)
	}
	if status, _ = call(t, c, "GET", path+"/m_999", "", nil); status != 404 {
		t.Fatalf("unknown media: got %d, want 404", status)
	}
	seeded := c.AddMedia("audio/ogg", []byte("ogg"))
	if status, got = call(t, c, "GET", path+"/"+seeded, "", nil); status != 200 || got != "ogg" {
		t.Fatalf("seeded media: got %d %q", status, got)
	}
}

func TestChannelIsolation403(t *testing.T) {
	c := newCore(t)
	status, body := call(t, c, "POST", "/channels/other/events", "application/json", fixture(t, "events/message-private-text.json"))
	if status != 403 || strings.TrimSpace(body) != strings.TrimSpace(string(fixture(t, "errors/channel-mismatch.json"))) {
		t.Fatalf("other channel: got %d %s, want 403 channel_mismatch", status, body)
	}
	status, _ = call(t, c, "GET", "/channels/other/media/m_1", "", nil)
	if status != 403 {
		t.Fatalf("other channel media: got %d, want 403", status)
	}

	req, _ := http.NewRequest("POST", c.URL()+evPath(c), bytes.NewReader(fixture(t, "events/message-private-text.json")))
	req.Header.Set("Authorization", "Bearer wrong")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 403 || len(b) != 0 {
		t.Fatalf("bad token: got %d %q, want bare 403", resp.StatusCode, b)
	}
	if n := len(c.Events()); n != 0 {
		t.Fatalf("rejected requests recorded %d events", n)
	}
}

func TestFailNextThenOK(t *testing.T) {
	c := newCore(t)
	c.FailNext(fakecore.RouteEvents, 2, 500, `{"error":"boom"}`)
	ev := fixture(t, "events/message-private-text.json")
	for i := 0; i < 2; i++ {
		status, body := call(t, c, "POST", evPath(c), "application/json", ev)
		if status != 500 || !strings.Contains(body, "boom") {
			t.Fatalf("attempt %d: got %d %s, want 500", i, status, body)
		}
	}
	if status, _ := call(t, c, "POST", evPath(c), "application/json", ev); status != 202 {
		t.Fatalf("third attempt: got %d, want 202", status)
	}
	if n := len(c.Events()); n != 1 {
		t.Fatalf("Events() = %d, want 1 (faulted attempts must not record)", n)
	}
	if got := c.Calls(fakecore.RouteEvents); got != 3 {
		t.Fatalf("Calls(events) = %d, want 3", got)
	}
	if _, ok := c.LastCall(fakecore.RouteEvents); !ok {
		t.Fatal("LastCall(events) not recorded")
	}
	// A fault on another route does not leak into events.
	c.FailNext(fakecore.RouteMedia, 1, 503, "")
	if status, _ := call(t, c, "POST", evPath(c), "application/json", []byte(`{}`)); status != 400 {
		t.Fatalf("events after media fault: got %d, want 400 from validation", status)
	}
}

func TestDownThenUp(t *testing.T) {
	c := newCore(t)
	ev := fixture(t, "events/message-private-text.json")
	c.Down()
	if status, _, err := try(c, "POST", evPath(c), "application/json", ev); err == nil {
		t.Fatalf("Down: got status %d, want a connection error", status)
	}
	c.Up()
	if status, _, err := try(c, "POST", evPath(c), "application/json", ev); err != nil || status != 202 {
		t.Fatalf("Up: got %d %v, want 202", status, err)
	}
}

func TestWaitEventsUnblocksOnArrival(t *testing.T) {
	c := newCore(t)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	done := make(chan []contract.Event, 1)
	go func() {
		evs, err := c.WaitEvents(ctx, 1)
		if err != nil {
			t.Error(err)
		}
		done <- evs
	}()
	select {
	case <-done:
		t.Fatal("WaitEvents returned before any event arrived")
	default:
	}
	call(t, c, "POST", evPath(c), "application/json", fixture(t, "events/message-private-text.json"))
	evs := <-done
	if len(evs) != 1 || *evs[0].EventID != "upd-88124" {
		t.Fatalf("WaitEvents = %v", evs)
	}

	cctx, ccancel := context.WithCancel(context.Background())
	ccancel()
	if _, err := c.WaitEvents(cctx, 5); err == nil {
		t.Fatal("WaitEvents with a cancelled context and too few events must fail")
	}
}
