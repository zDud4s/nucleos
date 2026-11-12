package cdp

import (
	"context"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

// fakeBrowser is a CDP server: enough of RFC 6455 to answer, and a scripted set of replies. It
// exists so this package is tested without Chrome, for the reason `search.Fake` exists in the web
// sidecar — a transport that needs a browser to be tested is a transport that is not tested.
type fakeBrowser struct {
	listener net.Listener
	URL      string

	mu       sync.Mutex
	received []message
	// reply is consulted for each command; returning nil means "no reply", for testing timeouts.
	reply func(message) *message
	conns []net.Conn
}

func newFakeBrowser(t *testing.T, reply func(message) *message) *fakeBrowser {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	browser := &fakeBrowser{
		listener: listener,
		URL:      "ws://" + listener.Addr().String() + "/devtools/browser/x",
		reply:    reply,
	}
	go browser.accept()
	t.Cleanup(browser.Close)
	return browser
}

func (f *fakeBrowser) accept() {
	for {
		conn, err := f.listener.Accept()
		if err != nil {
			return
		}
		f.mu.Lock()
		f.conns = append(f.conns, conn)
		f.mu.Unlock()
		go f.serve(conn)
	}
}

func (f *fakeBrowser) serve(conn net.Conn) {
	defer conn.Close()
	buf := make([]byte, 0, 1024)
	tmp := make([]byte, 512)
	for !strings.Contains(string(buf), "\r\n\r\n") {
		n, err := conn.Read(tmp)
		if err != nil {
			return
		}
		buf = append(buf, tmp[:n]...)
	}
	var key string
	for _, line := range strings.Split(string(buf), "\r\n") {
		if strings.HasPrefix(strings.ToLower(line), "sec-websocket-key:") {
			key = strings.TrimSpace(line[len("sec-websocket-key:"):])
		}
	}
	sum := sha1.Sum([]byte(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"))
	_, _ = io.WriteString(conn,
		"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"+
			"Sec-WebSocket-Accept: "+base64.StdEncoding.EncodeToString(sum[:])+"\r\n\r\n")

	for {
		payload, err := readServerFrame(conn)
		if err != nil {
			return
		}
		var msg message
		if err := json.Unmarshal(payload, &msg); err != nil {
			return
		}
		f.mu.Lock()
		f.received = append(f.received, msg)
		responder := f.reply
		f.mu.Unlock()
		if responder == nil {
			continue
		}
		if response := responder(msg); response != nil {
			f.send(conn, *response)
		}
	}
}

func (f *fakeBrowser) send(conn net.Conn, msg message) {
	encoded, err := json.Marshal(msg)
	if err != nil {
		return
	}
	// Server frames are unmasked.
	header := []byte{0x81}
	switch {
	case len(encoded) < 126:
		header = append(header, byte(len(encoded)))
	case len(encoded) < 1<<16:
		header = append(header, 126)
		header = binary.BigEndian.AppendUint16(header, uint16(len(encoded)))
	default:
		header = append(header, 127)
		header = binary.BigEndian.AppendUint64(header, uint64(len(encoded)))
	}
	_, _ = conn.Write(append(header, encoded...))
}

// Broadcast pushes an event to every connected client.
func (f *fakeBrowser) Broadcast(msg message) {
	f.mu.Lock()
	conns := append([]net.Conn(nil), f.conns...)
	f.mu.Unlock()
	for _, conn := range conns {
		f.send(conn, msg)
	}
}

func (f *fakeBrowser) Received() []message {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]message(nil), f.received...)
}

func (f *fakeBrowser) Close() {
	_ = f.listener.Close()
	f.mu.Lock()
	for _, conn := range f.conns {
		_ = conn.Close()
	}
	f.mu.Unlock()
}

func readServerFrame(conn net.Conn) ([]byte, error) {
	var head [2]byte
	if _, err := io.ReadFull(conn, head[:]); err != nil {
		return nil, err
	}
	masked := head[1]&0x80 != 0
	length := uint64(head[1] & 0x7F)
	switch length {
	case 126:
		var ext [2]byte
		if _, err := io.ReadFull(conn, ext[:]); err != nil {
			return nil, err
		}
		length = uint64(binary.BigEndian.Uint16(ext[:]))
	case 127:
		var ext [8]byte
		if _, err := io.ReadFull(conn, ext[:]); err != nil {
			return nil, err
		}
		length = binary.BigEndian.Uint64(ext[:])
	}
	var mask [4]byte
	if masked {
		if _, err := io.ReadFull(conn, mask[:]); err != nil {
			return nil, err
		}
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(conn, payload); err != nil {
		return nil, err
	}
	if masked {
		for i := range payload {
			payload[i] ^= mask[i%4]
		}
	}
	if head[0]&0x0F == opClose {
		return nil, io.EOF
	}
	return payload, nil
}

func echo(msg message) *message {
	return &message{ID: msg.ID, Result: json.RawMessage(`{"ok":true}`), SessionID: msg.SessionID}
}

func dial(t *testing.T, browser *fakeBrowser) *Conn {
	t.Helper()
	conn, err := Dial(browser.URL, 5*time.Second)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

// TestBrowserSessionIsTheZeroValueAndSendsNoSessionId.
//
// The browser session is where the fence lives (see the SessionID doc). On the wire it is the
// ABSENCE of sessionId, and a client that sent `"sessionId": ""` would be addressing a target named
// empty-string rather than the browser.
func TestBrowserSessionIsTheZeroValueAndSendsNoSessionId(t *testing.T) {
	browser := newFakeBrowser(t, echo)
	conn := dial(t, browser)

	if _, err := conn.Call(context.Background(), BrowserSession, "Fetch.enable", map[string]any{
		"patterns": []map[string]string{{"urlPattern": "*"}},
	}); err != nil {
		t.Fatalf("call: %v", err)
	}

	received := browser.Received()
	if len(received) != 1 {
		t.Fatalf("browser received %d messages", len(received))
	}
	if received[0].SessionID != BrowserSession {
		t.Fatalf("sessionId was %q, want the browser session", received[0].SessionID)
	}
	if received[0].Method != "Fetch.enable" {
		t.Fatalf("method: %q", received[0].Method)
	}
}

func TestAPageSessionIsAddressedExplicitly(t *testing.T) {
	browser := newFakeBrowser(t, echo)
	conn := dial(t, browser)

	if _, err := conn.Call(context.Background(), SessionID("S1"), "Page.enable", nil); err != nil {
		t.Fatalf("call: %v", err)
	}
	if got := browser.Received()[0].SessionID; got != "S1" {
		t.Fatalf("sessionId: %q", got)
	}
}

// TestRepliesAreMatchedToTheirCaller. One socket carries every session, so a reply landing in the
// wrong caller would silently mix two pages' answers together.
func TestRepliesAreMatchedToTheirCaller(t *testing.T) {
	// Answer out of order: the second command replies first.
	var mu sync.Mutex
	var held *message
	browser := newFakeBrowser(t, func(msg message) *message {
		mu.Lock()
		defer mu.Unlock()
		if held == nil {
			held = &message{ID: msg.ID, Result: json.RawMessage(`{"which":"first"}`)}
			return nil
		}
		return &message{ID: msg.ID, Result: json.RawMessage(`{"which":"second"}`)}
	})
	conn := dial(t, browser)

	results := make(chan string, 2)
	go func() {
		result, err := conn.Call(context.Background(), BrowserSession, "First", nil)
		if err != nil {
			results <- "error: " + err.Error()
			return
		}
		results <- string(result)
	}()
	// Give the first command time to be sent and held.
	time.Sleep(150 * time.Millisecond)
	second, err := conn.Call(context.Background(), BrowserSession, "Second", nil)
	if err != nil {
		t.Fatalf("second call: %v", err)
	}
	if !strings.Contains(string(second), "second") {
		t.Fatalf("the second caller got %s", second)
	}

	// Now release the first.
	mu.Lock()
	release := held
	mu.Unlock()
	browser.Broadcast(*release)

	select {
	case got := <-results:
		if !strings.Contains(got, "first") {
			t.Fatalf("the first caller got %s", got)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the first caller never got its reply")
	}
}

// TestAProtocolErrorIsNotATransportError. A wrong call and a browser that has gone away have
// completely different remedies, so they must not arrive as the same thing.
func TestAProtocolErrorIsNotATransportError(t *testing.T) {
	browser := newFakeBrowser(t, func(msg message) *message {
		return &message{ID: msg.ID, Error: &ProtocolError{Code: -32601, Message: "'X.y' wasn't found"}}
	})
	conn := dial(t, browser)

	_, err := conn.Call(context.Background(), BrowserSession, "X.y", nil)
	var protocolErr *ProtocolError
	if !errors.As(err, &protocolErr) {
		t.Fatalf("got %T (%v), want *ProtocolError", err, err)
	}
	if protocolErr.Code != -32601 {
		t.Fatalf("code: %d", protocolErr.Code)
	}
	if errors.Is(err, ErrClosed) {
		t.Fatal("a protocol error was reported as a closed connection")
	}
}

// TestEventsReachEveryHandlerWithTheirSession. The fence subscribes here, and it needs to know
// WHICH session a paused request belongs to in order to answer it.
func TestEventsReachEveryHandlerWithTheirSession(t *testing.T) {
	browser := newFakeBrowser(t, echo)
	conn := dial(t, browser)

	events := make(chan Event, 4)
	cancel := conn.OnEvent(func(e Event) { events <- e })

	browser.Broadcast(message{
		Method:    "Fetch.requestPaused",
		SessionID: "S9",
		Params:    json.RawMessage(`{"requestId":"R1"}`),
	})

	select {
	case event := <-events:
		if event.Method != "Fetch.requestPaused" {
			t.Fatalf("method: %s", event.Method)
		}
		if event.Session != "S9" {
			t.Fatalf("session: %q — the fence could not answer this request", event.Session)
		}
		if !strings.Contains(string(event.Params), "R1") {
			t.Fatalf("params: %s", event.Params)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no event arrived")
	}

	cancel()
	browser.Broadcast(message{Method: "Fetch.requestPaused", SessionID: "S9"})
	select {
	case event := <-events:
		t.Fatalf("a cancelled handler still fired: %v", event)
	case <-time.After(300 * time.Millisecond):
	}
}

// TestAHandlerMayCallBackIn. The fence answers every paused request from inside its own handler; if
// dispatch held a lock across the callback, that would deadlock on the first interception.
func TestAHandlerMayCallBackIn(t *testing.T) {
	browser := newFakeBrowser(t, echo)
	conn := dial(t, browser)

	answered := make(chan error, 1)
	conn.OnEvent(func(e Event) {
		if e.Method != "Fetch.requestPaused" {
			return
		}
		_, err := conn.Call(context.Background(), e.Session, "Fetch.continueRequest", map[string]any{
			"requestId": "R1",
		})
		answered <- err
	})
	browser.Broadcast(message{
		Method:    "Fetch.requestPaused",
		SessionID: "S1",
		Params:    json.RawMessage(`{"requestId":"R1"}`),
	})

	select {
	case err := <-answered:
		if err != nil {
			t.Fatalf("answering from inside the handler failed: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("deadlocked answering a paused request from inside the handler")
	}
}

func TestContextCancellationReleasesTheCaller(t *testing.T) {
	browser := newFakeBrowser(t, func(message) *message { return nil }) // never answers
	conn := dial(t, browser)

	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()
	_, err := conn.Call(ctx, BrowserSession, "Never.answers", nil)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("got %v, want a deadline error", err)
	}
}

// TestCallsAfterCloseAreRefusedNotHung. A browser that died must not leave the fence waiting.
func TestCallsAfterCloseAreRefusedNotHung(t *testing.T) {
	browser := newFakeBrowser(t, echo)
	conn := dial(t, browser)
	if err := conn.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}

	done := make(chan error, 1)
	go func() {
		_, err := conn.Call(context.Background(), BrowserSession, "Anything", nil)
		done <- err
	}()
	select {
	case err := <-done:
		if !errors.Is(err, ErrClosed) {
			t.Fatalf("got %v, want ErrClosed", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a call after close hung")
	}
}

// TestAnInFlightCallIsReleasedWhenTheBrowserDies is the crash case: a browser killed mid-command
// must not strand whoever was waiting.
func TestAnInFlightCallIsReleasedWhenTheBrowserDies(t *testing.T) {
	browser := newFakeBrowser(t, func(message) *message { return nil })
	conn := dial(t, browser)

	done := make(chan error, 1)
	go func() {
		_, err := conn.Call(context.Background(), BrowserSession, "Never.answers", nil)
		done <- err
	}()
	time.Sleep(200 * time.Millisecond)
	browser.Close()

	select {
	case err := <-done:
		if err == nil {
			t.Fatal("the call succeeded against a dead browser")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a call was stranded when the browser died")
	}
}

func TestDialRefusesANonLoopbackScheme(t *testing.T) {
	if _, err := Dial("wss://example.com/devtools", time.Second); err == nil {
		t.Fatal("dialled a wss:// endpoint")
	}
	if _, err := Dial("http://127.0.0.1:1/x", time.Second); err == nil {
		t.Fatal("dialled an http:// endpoint")
	}
}

// TestLargeMessagesSurvive covers the 16-bit and 64-bit length paths, which a screenshot reply
// exercises and a short unit test otherwise never would.
func TestLargeMessagesSurvive(t *testing.T) {
	big := strings.Repeat("x", 200000)
	browser := newFakeBrowser(t, func(msg message) *message {
		return &message{ID: msg.ID, Result: json.RawMessage(fmt.Sprintf(`{"data":%q}`, big))}
	})
	conn := dial(t, browser)

	result, err := conn.Call(context.Background(), BrowserSession, "Page.captureScreenshot", nil)
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	var payload struct {
		Data string `json:"data"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if len(payload.Data) != len(big) {
		t.Fatalf("got %d bytes, want %d", len(payload.Data), len(big))
	}
}
