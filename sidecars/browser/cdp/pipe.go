package cdp

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"sync"
)

// pipeConn is the DevTools pipe transport (--remote-debugging-pipe).
//
// Framing, from Chromium's content/browser/devtools/devtools_pipe_handler.cc: every message, in both
// directions, is a JSON text terminated by a single NUL byte ("ASCIIZ"). There is no length prefix and
// no escaping; JSON text never contains a raw NUL.
type pipeConn struct {
	r      *bufio.Reader
	rc     io.Closer // the reader's closer, when it has one
	w      io.WriteCloser
	writeM sync.Mutex
}

// NewPipe builds a Conn over the two ends of a DevTools pipe: r yields what the browser writes, w
// receives what it reads. Closing the Conn closes w and, when r is an io.Closer, r. EOF on r ends the
// Conn exactly as a closed WebSocket does.
func NewPipe(r io.Reader, w io.WriteCloser) *Conn {
	p := &pipeConn{w: w}
	if br, ok := r.(*bufio.Reader); ok {
		// Keep the caller's buffer: bytes it already read must not be lost.
		p.r = br
	} else {
		p.r = bufio.NewReader(r)
	}
	if c, ok := r.(io.Closer); ok {
		p.rc = c
	}
	return newConn(p)
}

// readMessage reads up to the next NUL and returns the message without it.
func (p *pipeConn) readMessage() ([]byte, error) {
	var message []byte
	for {
		chunk, err := p.r.ReadSlice(0)
		if len(message)+len(chunk) > maxFrame+1 {
			return nil, fmt.Errorf("cdp: pipe message exceeds %d bytes", maxFrame)
		}
		message = append(message, chunk...)
		switch {
		case err == nil:
			return message[:len(message)-1], nil
		case errors.Is(err, bufio.ErrBufferFull):
			continue
		default:
			return nil, err
		}
	}
}

// writeText sends payload and its terminating NUL in one Write, so concurrent writers cannot
// interleave and a reader never sees a message without its terminator.
func (p *pipeConn) writeText(payload []byte) error {
	buf := make([]byte, 0, len(payload)+1)
	buf = append(buf, payload...)
	buf = append(buf, 0)
	p.writeM.Lock()
	defer p.writeM.Unlock()
	_, err := p.w.Write(buf)
	return err
}

func (p *pipeConn) Close() error {
	err := p.w.Close()
	if p.rc != nil {
		if rerr := p.rc.Close(); err == nil {
			err = rerr
		}
	}
	return err
}
