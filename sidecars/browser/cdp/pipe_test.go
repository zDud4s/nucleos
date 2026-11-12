package cdp

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"testing"
	"time"
)

// pipePair wires a Conn built with NewPipe to a server side made of io.Pipe ends.
func pipePair(t *testing.T) (conn *Conn, serverIn *bufio.Reader, serverOut io.WriteCloser) {
	t.Helper()
	clientToServerR, clientToServerW := io.Pipe()
	serverToClientR, serverToClientW := io.Pipe()
	conn = NewPipe(serverToClientR, clientToServerW)
	t.Cleanup(func() {
		_ = conn.Close()
		_ = serverToClientW.Close()
		_ = clientToServerR.Close()
	})
	return conn, bufio.NewReader(clientToServerR), serverToClientW
}

func TestPipeConnCallRoundTrip(t *testing.T) {
	conn, in, out := pipePair(t)
	seen := make(chan []byte, 1)
	go func() {
		raw, err := in.ReadBytes(0)
		if err != nil {
			return
		}
		seen <- raw
		var msg struct {
			ID int64 `json:"id"`
		}
		if err := json.Unmarshal(raw[:len(raw)-1], &msg); err != nil {
			return
		}
		_, _ = out.Write([]byte(fmt.Sprintf(`{"id":%d,"result":{"ok":true}}`, msg.ID) + "\x00"))
	}()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	result, err := conn.Call(ctx, BrowserSession, "Browser.getVersion", nil)
	if err != nil {
		t.Fatalf("call: %v", err)
	}
	var got struct {
		OK bool `json:"ok"`
	}
	if err := json.Unmarshal(result, &got); err != nil || !got.OK {
		t.Fatalf("result = %s (%v)", result, err)
	}

	raw := <-seen
	if raw[len(raw)-1] != 0 || bytes.Count(raw, []byte{0}) != 1 {
		t.Fatalf("message must end with exactly one NUL and contain no other: %q", raw)
	}
}

func TestPipeConnDeliversEventsWithSession(t *testing.T) {
	conn, _, out := pipePair(t)
	events := make(chan Event, 1)
	conn.OnEvent(func(e Event) { events <- e })

	go func() {
		_, _ = out.Write([]byte(`{"method":"Page.loadEventFired","sessionId":"S1","params":{}}` + "\x00"))
	}()

	select {
	case e := <-events:
		if e.Method != "Page.loadEventFired" || e.Session != "S1" {
			t.Fatalf("event = %+v", e)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("no event delivered")
	}
}

func TestPipeConnClosesWhenPeerHangsUp(t *testing.T) {
	conn, _, out := pipePair(t)
	_ = out.Close()

	select {
	case <-conn.Done():
	case <-time.After(2 * time.Second):
		t.Fatal("Done did not close after the peer hung up")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if _, err := conn.Call(ctx, BrowserSession, "Browser.getVersion", nil); !errors.Is(err, ErrClosed) {
		t.Fatalf("got %v, want ErrClosed", err)
	}
}

func TestPipeConnSkipsAnEmptyFrame(t *testing.T) {
	conn, _, out := pipePair(t)
	events := make(chan Event, 1)
	conn.OnEvent(func(e Event) { events <- e })

	go func() {
		// A bare NUL, then a real message.
		_, _ = out.Write([]byte("\x00" + `{"method":"Page.loadEventFired","params":{}}` + "\x00"))
	}()

	select {
	case e := <-events:
		if e.Method != "Page.loadEventFired" {
			t.Fatalf("event = %+v", e)
		}
	case <-conn.Done():
		t.Fatal("an empty frame ended the connection")
	case <-time.After(5 * time.Second):
		t.Fatal("no event delivered after the empty frame")
	}
}
