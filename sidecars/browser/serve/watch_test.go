// §spec browser-ao-vivo

package serve

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// watchServer builds the mux the way Serve does for /watch: the driver is asked whether it is a
// Watcher, and the handler is given the answer (nil when it is not).
func watchServer(t *testing.T, driver browser.Driver, wrap func(http.HandlerFunc) http.HandlerFunc) *httptest.Server {
	t.Helper()
	watcher, _ := driver.(browser.Watcher)
	handler := authorized(token, watchHandler(watcher))
	if wrap != nil {
		handler = wrap(handler)
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/watch", handler)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

// scriptedWatcher is a Fake that can be watched: it pushes its frames, then ends the way its script says.
type scriptedWatcher struct {
	*browser.Fake
	// beforeEach runs before the frame at that index is sent, so a test can hold a frame back until
	// the client has proved it received the one before.
	beforeEach func(i int)
	frames     [][]byte
	end        error
}

func (s *scriptedWatcher) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	for i, jpeg := range s.frames {
		if s.beforeEach != nil {
			s.beforeEach(i)
		}
		sink(browser.Frame{JPEG: jpeg})
	}
	return s.end
}

func rawWatch(t *testing.T, server *httptest.Server, session string) *http.Response {
	t.Helper()
	return post(t, server, "/watch", SessionRequest{SessionID: session}, true)
}

func uint32BE(n int) []byte {
	var out [4]byte
	binary.BigEndian.PutUint32(out[:], uint32(n))
	return out[:]
}

// TestWatchRecordRoundTrip pins the wire format the shell parses: one kind byte, a four-byte
// big-endian length, then the body.
func TestWatchRecordRoundTrip(t *testing.T) {
	var wire bytes.Buffer
	if err := WriteRecord(&wire, RecordFrame, []byte("jpeg-bytes")); err != nil {
		t.Fatalf("write frame: %v", err)
	}
	if err := WriteRecord(&wire, RecordEnd, []byte(`{"reason":"closed"}`)); err != nil {
		t.Fatalf("write end: %v", err)
	}
	if err := WriteRecord(&wire, RecordFrame, nil); err != nil {
		t.Fatalf("write empty: %v", err)
	}

	if RecordFrame != 'F' || RecordEnd != 'E' {
		t.Fatalf("record kinds drifted: %q %q", RecordFrame, RecordEnd)
	}
	want := append([]byte{'F'}, uint32BE(len("jpeg-bytes"))...)
	if got := wire.Bytes()[:5]; !bytes.Equal(got, want) {
		t.Fatalf("header = % x, want % x", got, want)
	}

	kind, body, err := ReadRecord(&wire)
	if err != nil || kind != RecordFrame || string(body) != "jpeg-bytes" {
		t.Fatalf("first record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(&wire)
	if err != nil || kind != RecordEnd || string(body) != `{"reason":"closed"}` {
		t.Fatalf("second record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(&wire)
	if err != nil || kind != RecordFrame || len(body) != 0 {
		t.Fatalf("empty record = %q %q %v", kind, body, err)
	}
	if _, _, err := ReadRecord(&wire); !errors.Is(err, io.EOF) {
		t.Fatalf("after the last record: %v, want io.EOF", err)
	}
}

func TestWatchReadRecordRefusesWhatItCannotTrust(t *testing.T) {
	// A length over 16 MiB is refused before anything is allocated for it.
	huge := append([]byte{'F'}, uint32BE(16<<20+1)...)
	if _, _, err := ReadRecord(bytes.NewReader(huge)); err == nil {
		t.Fatal("a 16 MiB + 1 record was accepted")
	}
	// A body cut short is an error, not a short frame.
	cut := append(append([]byte{'F'}, uint32BE(10)...), []byte("abc")...)
	if _, _, err := ReadRecord(bytes.NewReader(cut)); err == nil {
		t.Fatal("a truncated record was accepted")
	}
}

// TestWatchStreamsFramesThenOneEndRecord. Frames must arrive while the watch is still running — the
// second frame is held back until the client has read the first, which a buffered-until-the-end
// response could never satisfy.
func TestWatchStreamsFramesThenOneEndRecord(t *testing.T) {
	firstRead := make(chan struct{})
	watcher := &scriptedWatcher{
		Fake:   &browser.Fake{FenceAttached: true},
		frames: [][]byte{[]byte("one"), []byte("two")},
		end:    browser.WatchEnded{Reason: browser.EndWheel},
		beforeEach: func(i int) {
			if i == 1 {
				select {
				case <-firstRead:
				case <-time.After(5 * time.Second):
				}
			}
		},
	}
	server := watchServer(t, watcher, nil)

	response := rawWatch(t, server, "s1")
	if response.StatusCode != http.StatusOK {
		t.Fatalf("got %d, want 200", response.StatusCode)
	}
	if got := response.Header.Get("Content-Type"); got != "application/octet-stream" {
		t.Fatalf("content type = %q", got)
	}

	reader := bufio.NewReader(response.Body)
	kind, body, err := ReadRecord(reader)
	if err != nil || kind != RecordFrame || string(body) != "one" {
		t.Fatalf("first record = %q %q %v", kind, body, err)
	}
	close(firstRead)
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordFrame || string(body) != "two" {
		t.Fatalf("second record = %q %q %v", kind, body, err)
	}
	kind, body, err = ReadRecord(reader)
	if err != nil || kind != RecordEnd {
		t.Fatalf("third record = %q %q %v, want the end record", kind, body, err)
	}
	var end struct {
		Reason string `json:"reason"`
	}
	if err := json.Unmarshal(body, &end); err != nil || end.Reason != "wheel" {
		t.Fatalf("end body = %q (%v), want reason wheel", body, err)
	}
	if _, _, err := ReadRecord(reader); !errors.Is(err, io.EOF) {
		t.Fatalf("after the end record: %v, want EOF — exactly one end record", err)
	}
}

// A watch that ends for no stated reason while the client is still there is "gone": the browser went
// away under the viewer.
func TestWatchAnUnexplainedEndIsGone(t *testing.T) {
	watcher := &scriptedWatcher{
		Fake:   &browser.Fake{FenceAttached: true},
		frames: [][]byte{[]byte("one")},
		end:    errors.New("cdp connection lost"),
	}
	server := watchServer(t, watcher, nil)

	reader := bufio.NewReader(rawWatch(t, server, "s1").Body)
	if kind, _, err := ReadRecord(reader); err != nil || kind != RecordFrame {
		t.Fatalf("first record = %q %v", kind, err)
	}
	kind, body, err := ReadRecord(reader)
	if err != nil || kind != RecordEnd || !strings.Contains(string(body), `"gone"`) {
		t.Fatalf("end record = %q %q %v, want reason gone", kind, body, err)
	}
}

// halfADriver is not a Watcher, and a driver that cannot stream says so: 501, not 500 — nothing failed.
func TestWatchWithoutAWatcherIs501(t *testing.T) {
	server := watchServer(t, halfADriver{}, nil)
	response := rawWatch(t, server, "s1")
	if response.StatusCode != http.StatusNotImplemented {
		t.Fatalf("got %d, want 501", response.StatusCode)
	}
}

// Before any frame the route has not committed to a stream, so a refusal is an ordinary status.
func TestWatchWhileAPersonDrivesIs409(t *testing.T) {
	watcher := &scriptedWatcher{
		Fake: &browser.Fake{FenceAttached: true},
		end:  fmt.Errorf("%w: s1", browser.ErrPersonIsDriving),
	}
	server := watchServer(t, watcher, nil)
	if got := rawWatch(t, server, "s1").StatusCode; got != http.StatusConflict {
		t.Fatalf("got %d, want 409", got)
	}

	missing := &scriptedWatcher{Fake: &browser.Fake{FenceAttached: true}, end: browser.ErrNoSuchSession}
	server = watchServer(t, missing, nil)
	if got := rawWatch(t, server, "s1").StatusCode; got != http.StatusNotFound {
		t.Fatalf("unknown session: got %d, want 404", got)
	}
}

func TestWatchRefusesAnEmptySessionAndAMissingToken(t *testing.T) {
	watcher := &scriptedWatcher{Fake: &browser.Fake{FenceAttached: true}}
	server := watchServer(t, watcher, nil)
	if got := rawWatch(t, server, "").StatusCode; got != http.StatusBadRequest {
		t.Fatalf("empty session: got %d, want 400", got)
	}
	if got := post(t, server, "/watch", SessionRequest{SessionID: "s1"}, false).StatusCode; got != http.StatusUnauthorized {
		t.Fatalf("no token: got %d, want 401", got)
	}
}

// stallingWatcher pushes 1 MiB frames from its own goroutine, so a sink that blocks on a client that
// never reads cannot stop Watch itself from being ended from outside — which is the contract on a
// Watcher ("a blocked sink must never stall the browser").
type stallingWatcher struct {
	*browser.Fake
	end      chan struct{}
	started  atomic.Int64
	returned atomic.Int64
}

func (s *stallingWatcher) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	done := make(chan struct{})
	defer close(done)
	frame := browser.Frame{JPEG: make([]byte, 1<<20)}
	go func() {
		for {
			select {
			case <-done:
				return
			default:
			}
			s.started.Add(1)
			sink(frame)
			s.returned.Add(1)
		}
	}()
	select {
	case <-s.end:
	case <-ctx.Done():
	}
	return browser.WatchEnded{Reason: browser.EndWheel}
}

// TestWatchStalledClientIsDroppedWithinTheWriteDeadline. The client never reads, so the socket fills
// and the sink blocks inside a write. Ending the watch must still free the handler: the one-second
// write deadline is what turns a blocked write into an error.
func TestWatchStalledClientIsDroppedWithinTheWriteDeadline(t *testing.T) {
	watcher := &stallingWatcher{Fake: &browser.Fake{FenceAttached: true}, end: make(chan struct{})}
	handlerDone := make(chan struct{})
	server := watchServer(t, watcher, func(next http.HandlerFunc) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			defer close(handlerDone)
			next(w, r)
		}
	})

	conn, err := net.Dial("tcp", strings.TrimPrefix(server.URL, "http://"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	if tcp, ok := conn.(*net.TCPConn); ok {
		_ = tcp.SetReadBuffer(4096)
	}
	body := `{"session_id":"s1"}`
	request := fmt.Sprintf("POST /watch HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s",
		token, len(body), body)
	if _, err := io.WriteString(conn, request); err != nil {
		t.Fatal(err)
	}

	// Stalled means a sink call has been in flight, unmoved, for a good while: the socket is full.
	deadline := time.Now().Add(20 * time.Second)
	var since time.Time
	var seen int64 = -1
	for {
		if time.Now().After(deadline) {
			t.Fatal("the socket never filled; the test could not stall the client")
		}
		started, returned := watcher.started.Load(), watcher.returned.Load()
		if started > returned && started == seen {
			if since.IsZero() {
				since = time.Now()
			}
			if time.Since(since) > 500*time.Millisecond {
				break
			}
		} else {
			since = time.Time{}
		}
		seen = started
		time.Sleep(50 * time.Millisecond)
	}

	ended := time.Now()
	close(watcher.end)
	select {
	case <-handlerDone:
	case <-time.After(2500 * time.Millisecond):
		t.Fatalf("the handler was still stuck %v after the watch ended", time.Since(ended))
	}
}
