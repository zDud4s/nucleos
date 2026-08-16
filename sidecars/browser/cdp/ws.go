// Package cdp speaks the Chrome DevTools Protocol.
//
// # Why the WebSocket is hand-rolled
//
// CDP is text frames over one long-lived socket, and this package needs exactly that: no
// compression, no subprotocols, no fragmentation of our own. A dependency would bring a general
// WebSocket implementation to do one specific thing, in the process whose whole job is to be the
// narrow place where a hostile page meets our code. The spike proved this framing works against
// Chrome 151 before a line of it was written here.
package cdp

import (
	"bufio"
	"crypto/rand"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"net/url"
	"strings"
	"sync"
	"time"
)

const (
	opText  = 0x1
	opClose = 0x8
	opPing  = 0x9
	opPong  = 0xA
)

// maxFrame bounds one incoming message. A CDP reply can be large — a snapshot, a screenshot — but
// not unbounded, and the sender is a browser rendering someone else's page.
const maxFrame = 64 << 20

type wsConn struct {
	conn net.Conn
	read *bufio.Reader

	// writeMu serialises writes. Frames must not interleave on the wire, and Call is used from
	// several goroutines at once.
	writeMu sync.Mutex
}

func dialWS(rawURL string, timeout time.Duration) (*wsConn, error) {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return nil, fmt.Errorf("cdp: bad websocket url %q: %w", rawURL, err)
	}
	if parsed.Scheme != "ws" {
		// The endpoint is on loopback, always. A wss:// url here would mean the daemon was pointed
		// somewhere off this machine, which is not a configuration this pillar has.
		return nil, fmt.Errorf("cdp: expected a ws:// url, got %q", parsed.Scheme)
	}
	host := parsed.Host
	if parsed.Port() == "" {
		host = net.JoinHostPort(host, "80")
	}
	conn, err := net.DialTimeout("tcp", host, timeout)
	if err != nil {
		return nil, fmt.Errorf("cdp: dialling %s: %w", host, err)
	}

	key := make([]byte, 16)
	if _, err := rand.Read(key); err != nil {
		conn.Close()
		return nil, err
	}
	path := parsed.RequestURI()
	if path == "" {
		path = "/"
	}
	request := "GET " + path + " HTTP/1.1\r\n" +
		"Host: " + parsed.Host + "\r\n" +
		"Upgrade: websocket\r\n" +
		"Connection: Upgrade\r\n" +
		"Sec-WebSocket-Key: " + base64.StdEncoding.EncodeToString(key) + "\r\n" +
		"Sec-WebSocket-Version: 13\r\n\r\n"
	if err := conn.SetDeadline(time.Now().Add(timeout)); err != nil {
		conn.Close()
		return nil, err
	}
	if _, err := io.WriteString(conn, request); err != nil {
		conn.Close()
		return nil, fmt.Errorf("cdp: sending handshake: %w", err)
	}

	reader := bufio.NewReaderSize(conn, 64<<10)
	status, err := reader.ReadString('\n')
	if err != nil {
		conn.Close()
		return nil, fmt.Errorf("cdp: reading handshake: %w", err)
	}
	if !strings.Contains(status, " 101") {
		conn.Close()
		return nil, fmt.Errorf("cdp: handshake refused: %s", strings.TrimSpace(status))
	}
	// Drain the remaining headers.
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			conn.Close()
			return nil, fmt.Errorf("cdp: reading handshake headers: %w", err)
		}
		if strings.TrimSpace(line) == "" {
			break
		}
	}
	// The handshake had a deadline; the session must not.
	if err := conn.SetDeadline(time.Time{}); err != nil {
		conn.Close()
		return nil, err
	}
	return &wsConn{conn: conn, read: reader}, nil
}

// writeText sends one masked text frame. Client frames MUST be masked (RFC 6455 §5.3).
func (w *wsConn) writeText(payload []byte) error {
	w.writeMu.Lock()
	defer w.writeMu.Unlock()
	return w.writeFrame(opText, payload)
}

func (w *wsConn) writeFrame(opcode byte, payload []byte) error {
	header := make([]byte, 0, 14)
	header = append(header, 0x80|opcode)
	length := len(payload)
	switch {
	case length < 126:
		header = append(header, 0x80|byte(length))
	case length < 1<<16:
		header = append(header, 0x80|126)
		header = binary.BigEndian.AppendUint16(header, uint16(length))
	default:
		header = append(header, 0x80|127)
		header = binary.BigEndian.AppendUint64(header, uint64(length))
	}
	var mask [4]byte
	if _, err := rand.Read(mask[:]); err != nil {
		return err
	}
	header = append(header, mask[:]...)
	masked := make([]byte, length)
	for i := range payload {
		masked[i] = payload[i] ^ mask[i%4]
	}
	if _, err := w.conn.Write(header); err != nil {
		return err
	}
	_, err := w.conn.Write(masked)
	return err
}

// readMessage returns the next complete text message, answering pings and skipping pongs.
func (w *wsConn) readMessage() ([]byte, error) {
	var message []byte
	for {
		var head [2]byte
		if _, err := io.ReadFull(w.read, head[:]); err != nil {
			return nil, err
		}
		final := head[0]&0x80 != 0
		opcode := head[0] & 0x0F
		masked := head[1]&0x80 != 0
		length := uint64(head[1] & 0x7F)
		switch length {
		case 126:
			var extended [2]byte
			if _, err := io.ReadFull(w.read, extended[:]); err != nil {
				return nil, err
			}
			length = uint64(binary.BigEndian.Uint16(extended[:]))
		case 127:
			var extended [8]byte
			if _, err := io.ReadFull(w.read, extended[:]); err != nil {
				return nil, err
			}
			length = binary.BigEndian.Uint64(extended[:])
		}
		if length > maxFrame || uint64(len(message))+length > maxFrame {
			return nil, fmt.Errorf("cdp: frame of %d bytes exceeds the ceiling", length)
		}
		var mask [4]byte
		if masked {
			if _, err := io.ReadFull(w.read, mask[:]); err != nil {
				return nil, err
			}
		}
		payload := make([]byte, length)
		if _, err := io.ReadFull(w.read, payload); err != nil {
			return nil, err
		}
		if masked {
			for i := range payload {
				payload[i] ^= mask[i%4]
			}
		}

		switch opcode {
		case opPing:
			w.writeMu.Lock()
			err := w.writeFrame(opPong, payload)
			w.writeMu.Unlock()
			if err != nil {
				return nil, err
			}
			continue
		case opPong:
			continue
		case opClose:
			return nil, io.EOF
		}

		message = append(message, payload...)
		if final {
			return message, nil
		}
	}
}

func (w *wsConn) Close() error {
	w.writeMu.Lock()
	_ = w.writeFrame(opClose, nil)
	w.writeMu.Unlock()
	return w.conn.Close()
}
