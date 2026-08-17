// Package cdptest is a fake browser that speaks CDP over a real socket.
//
// It exists so the driver can be tested without Chrome, for the reason `search.Fake` exists in the
// web sidecar: a browser pillar that needs a browser to be tested is a browser pillar that is not
// tested. It is a real listener and a real WebSocket, not a mock of the transport — the ordering
// this package is used to assert (the fence before the navigation) is only meaningful if the
// messages actually go over a wire in the order the driver sent them.
package cdptest

import (
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
)

// Call is one command the driver sent.
type Call struct {
	ID      int64
	Method  string
	Session string
	Params  json.RawMessage
}

// Handler answers a command. Returning an error makes the browser reply with a CDP protocol error,
// which is how a test simulates "Fetch.enable is not available" — the case spec §6.2a turns into a
// refusal to navigate.
type Handler func(call Call) (any, error)

// Browser is a fake CDP endpoint.
type Browser struct {
	listener net.Listener
	// URL is the WebSocket debugger url to hand to cdp.Dial.
	URL string

	mu       sync.Mutex
	calls    []Call
	handlers map[string]Handler
	fallback Handler
	conns    []net.Conn
}

// Start listens on loopback.
func Start() (*Browser, error) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	browser := &Browser{
		listener: listener,
		URL:      "ws://" + listener.Addr().String() + "/devtools/browser/fake",
		handlers: map[string]Handler{},
	}
	go browser.accept()
	return browser, nil
}

// Handle registers an answer for one method.
func (b *Browser) Handle(method string, handler Handler) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.handlers[method] = handler
}

// HandleDefault answers anything without a specific handler. Without one, unknown methods get an
// empty result — a browser that answered nothing would hang every caller.
func (b *Browser) HandleDefault(handler Handler) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.fallback = handler
}

// Calls returns every command received, in order. The ORDER is the point: the driver must enable
// the fence before it navigates, and a test that only checked both happened would pass on a driver
// that did them backwards.
func (b *Browser) Calls() []Call {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]Call(nil), b.calls...)
}

// Methods returns just the method names, in order.
func (b *Browser) Methods() []string {
	calls := b.Calls()
	out := make([]string, 0, len(calls))
	for _, call := range calls {
		out = append(out, call.Method)
	}
	return out
}

// IndexOf reports the position of the first call to a method, or -1.
func (b *Browser) IndexOf(method string) int {
	for i, name := range b.Methods() {
		if name == method {
			return i
		}
	}
	return -1
}

// Emit pushes an event to every connected client.
func (b *Browser) Emit(session, method string, params any) {
	encoded, err := json.Marshal(params)
	if err != nil {
		return
	}
	payload, err := json.Marshal(map[string]any{
		"method":    method,
		"params":    json.RawMessage(encoded),
		"sessionId": session,
	})
	if err != nil {
		return
	}
	b.mu.Lock()
	conns := append([]net.Conn(nil), b.conns...)
	b.mu.Unlock()
	for _, conn := range conns {
		writeFrame(conn, payload)
	}
}

// Close stops the fake.
func (b *Browser) Close() {
	_ = b.listener.Close()
	b.mu.Lock()
	for _, conn := range b.conns {
		_ = conn.Close()
	}
	b.conns = nil
	b.mu.Unlock()
}

func (b *Browser) accept() {
	for {
		conn, err := b.listener.Accept()
		if err != nil {
			return
		}
		b.mu.Lock()
		b.conns = append(b.conns, conn)
		b.mu.Unlock()
		go b.serve(conn)
	}
}

func (b *Browser) serve(conn net.Conn) {
	defer conn.Close()
	if err := handshake(conn); err != nil {
		return
	}
	for {
		payload, err := readFrame(conn)
		if err != nil {
			return
		}
		var incoming struct {
			ID        int64           `json:"id"`
			Method    string          `json:"method"`
			Params    json.RawMessage `json:"params"`
			SessionID string          `json:"sessionId"`
		}
		if err := json.Unmarshal(payload, &incoming); err != nil {
			return
		}
		call := Call{
			ID:      incoming.ID,
			Method:  incoming.Method,
			Session: incoming.SessionID,
			Params:  incoming.Params,
		}
		b.mu.Lock()
		b.calls = append(b.calls, call)
		handler, ok := b.handlers[incoming.Method]
		if !ok {
			handler = b.fallback
		}
		b.mu.Unlock()

		reply := map[string]any{"id": incoming.ID}
		if incoming.SessionID != "" {
			reply["sessionId"] = incoming.SessionID
		}
		if handler == nil {
			reply["result"] = map[string]any{}
		} else if result, err := handler(call); err != nil {
			reply["error"] = map[string]any{"code": -32000, "message": err.Error()}
		} else {
			if result == nil {
				result = map[string]any{}
			}
			reply["result"] = result
		}
		encoded, err := json.Marshal(reply)
		if err != nil {
			return
		}
		writeFrame(conn, encoded)
	}
}

func handshake(conn net.Conn) error {
	buf := make([]byte, 0, 1024)
	tmp := make([]byte, 512)
	for !strings.Contains(string(buf), "\r\n\r\n") {
		n, err := conn.Read(tmp)
		if err != nil {
			return err
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
	_, err := io.WriteString(conn,
		"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"+
			"Sec-WebSocket-Accept: "+base64.StdEncoding.EncodeToString(sum[:])+"\r\n\r\n")
	return err
}

// writeFrame sends an unmasked text frame, as a server must.
func writeFrame(conn net.Conn, payload []byte) {
	header := []byte{0x81}
	switch {
	case len(payload) < 126:
		header = append(header, byte(len(payload)))
	case len(payload) < 1<<16:
		header = append(header, 126)
		header = binary.BigEndian.AppendUint16(header, uint16(len(payload)))
	default:
		header = append(header, 127)
		header = binary.BigEndian.AppendUint64(header, uint64(len(payload)))
	}
	_, _ = conn.Write(append(header, payload...))
}

func readFrame(conn net.Conn) ([]byte, error) {
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
	if head[0]&0x0F == 0x8 {
		return nil, fmt.Errorf("cdptest: peer closed")
	}
	return payload, nil
}
