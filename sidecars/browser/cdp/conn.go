package cdp

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"time"
)

// SessionID addresses one attached target. The zero value is the BROWSER session.
//
// # This distinction is the spike's most consequential finding
//
// CDP has two levels, and the spec originally hung the fence on the wrong one. With Fetch enabled on
// a PAGE session, a service worker's script fetch never appears at all: the origin serves it and the
// worker installs. With Fetch enabled on the BROWSER session, the same request is intercepted and
// the registration never happens. It was never a limit of Chrome — it was where the fence hung.
//
// So this is a named type with a named zero value, and not a bare string. `Call(ctx, "", ...)` at a
// call site is a coin flip for the reader; `Call(ctx, cdp.BrowserSession, ...)` is a statement.
type SessionID string

// BrowserSession is the browser-level session: no target, the whole browser. Where the fence lives.
const BrowserSession SessionID = ""

// Event is one CDP notification.
type Event struct {
	Method  string
	Session SessionID
	Params  json.RawMessage
}

// ProtocolError is an error the browser reported, as opposed to one the transport had. They are kept
// apart because the remedies differ completely: a protocol error is a wrong call, a transport error
// is a browser that has gone away.
type ProtocolError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Data    string `json:"data,omitempty"`
}

func (e *ProtocolError) Error() string {
	if e.Data != "" {
		return fmt.Sprintf("cdp: %s (%d): %s", e.Message, e.Code, e.Data)
	}
	return fmt.Sprintf("cdp: %s (%d)", e.Message, e.Code)
}

// ErrClosed is returned once the connection is gone.
var ErrClosed = errors.New("cdp: connection closed")

type message struct {
	ID        int64           `json:"id,omitempty"`
	Method    string          `json:"method,omitempty"`
	Params    json.RawMessage `json:"params,omitempty"`
	Result    json.RawMessage `json:"result,omitempty"`
	Error     *ProtocolError  `json:"error,omitempty"`
	SessionID SessionID       `json:"sessionId,omitempty"`
}

// Conn is one connection to a browser, multiplexing every session over it.
type Conn struct {
	ws *wsConn

	nextID atomic.Int64

	mu       sync.Mutex
	pending  map[int64]chan *message
	handlers map[int]func(Event)
	nextHnd  int
	closeErr error
	closed   bool
	done     chan struct{}

	// Events are handed to a separate goroutine through this queue, and the read loop never runs a
	// handler itself.
	//
	// The first version dispatched inline and deadlocked on its own test: the fence's handler
	// answers a paused request with a Call, Call waits for the reply, and the reply can only be
	// delivered by the read loop — which was sitting inside the handler. Every interception would
	// have hung the browser on the first request.
	//
	// The queue is a slice rather than a buffered channel because the read loop must NEVER block:
	// a full channel would put the deadlock back under load, and dropping an event is not an option
	// when the event is a paused request. An unanswered pause wedges the renderer, which the spike
	// demonstrated by accident and spent a while misreading as "the navigation was blocked".
	queue     []Event
	queueCond *sync.Cond
}

// Dial connects to a browser's WebSocket debugger url.
func Dial(wsURL string, timeout time.Duration) (*Conn, error) {
	ws, err := dialWS(wsURL, timeout)
	if err != nil {
		return nil, err
	}
	conn := &Conn{
		ws:       ws,
		pending:  map[int64]chan *message{},
		handlers: map[int]func(Event){},
		done:     make(chan struct{}),
	}
	conn.queueCond = sync.NewCond(&conn.mu)
	go conn.readLoop()
	go conn.dispatchLoop()
	return conn, nil
}

// dispatchLoop runs handlers, in order, on a goroutine that is not the read loop. See the queue
// field for why that separation is load-bearing rather than tidiness.
func (c *Conn) dispatchLoop() {
	for {
		c.mu.Lock()
		for len(c.queue) == 0 && !c.closed {
			c.queueCond.Wait()
		}
		if len(c.queue) == 0 && c.closed {
			c.mu.Unlock()
			return
		}
		event := c.queue[0]
		c.queue = c.queue[1:]
		handlers := make([]func(Event), 0, len(c.handlers))
		for _, handler := range c.handlers {
			handlers = append(handlers, handler)
		}
		c.mu.Unlock()

		for _, handler := range handlers {
			handler(event)
		}
	}
}

func (c *Conn) readLoop() {
	for {
		raw, err := c.ws.readMessage()
		if err != nil {
			c.shutdown(err)
			return
		}
		var msg message
		if err := json.Unmarshal(raw, &msg); err != nil {
			// A browser that sends something unparseable is a browser we can no longer reason
			// about; carrying on would mean guessing.
			c.shutdown(fmt.Errorf("cdp: unreadable message: %w", err))
			return
		}
		if msg.ID != 0 {
			c.mu.Lock()
			waiter, ok := c.pending[msg.ID]
			delete(c.pending, msg.ID)
			c.mu.Unlock()
			if ok {
				waiter <- &msg
			}
			continue
		}
		if msg.Method == "" {
			continue
		}
		// Queue and carry on. The read loop's only job is to keep reading — it is the one goroutine
		// that can deliver a reply, so anything that blocks it blocks every caller at once.
		c.mu.Lock()
		c.queue = append(c.queue, Event{
			Method:  msg.Method,
			Session: msg.SessionID,
			Params:  msg.Params,
		})
		c.queueCond.Signal()
		c.mu.Unlock()
	}
}

func (c *Conn) shutdown(err error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return
	}
	c.closed = true
	c.closeErr = err
	waiters := c.pending
	c.pending = map[int64]chan *message{}
	// Wake the dispatcher so it can drain what is left and exit, rather than sitting on the
	// condition for ever.
	c.queueCond.Broadcast()
	c.mu.Unlock()

	close(c.done)
	for _, waiter := range waiters {
		close(waiter)
	}
	_ = c.ws.Close()
}

// Call sends a command and waits for its reply.
//
// `session` selects the level: [BrowserSession] for the browser, or an attached target's id. See
// the SessionID doc for why that choice is load-bearing rather than a detail.
func (c *Conn) Call(ctx context.Context, session SessionID, method string, params any) (json.RawMessage, error) {
	var encoded json.RawMessage
	if params != nil {
		raw, err := json.Marshal(params)
		if err != nil {
			return nil, fmt.Errorf("cdp: encoding %s params: %w", method, err)
		}
		encoded = raw
	}

	id := c.nextID.Add(1)
	waiter := make(chan *message, 1)

	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, c.closeReason()
	}
	c.pending[id] = waiter
	c.mu.Unlock()

	payload, err := json.Marshal(message{ID: id, Method: method, Params: encoded, SessionID: session})
	if err != nil {
		return nil, err
	}
	if err := c.ws.writeText(payload); err != nil {
		c.mu.Lock()
		delete(c.pending, id)
		c.mu.Unlock()
		return nil, fmt.Errorf("cdp: sending %s: %w", method, err)
	}

	select {
	case reply, ok := <-waiter:
		if !ok {
			return nil, c.closeReason()
		}
		if reply.Error != nil {
			return nil, reply.Error
		}
		return reply.Result, nil
	case <-ctx.Done():
		c.mu.Lock()
		delete(c.pending, id)
		c.mu.Unlock()
		return nil, ctx.Err()
	case <-c.done:
		return nil, c.closeReason()
	}
}

func (c *Conn) closeReason() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closeErr != nil {
		return fmt.Errorf("%w: %v", ErrClosed, c.closeErr)
	}
	return ErrClosed
}

// OnEvent registers a handler for every event on every session. The returned function removes it.
//
// Deliberately unfiltered: the fence has to see everything, and a subscription that filtered by
// method would make "the event we forgot to subscribe to" a silent hole rather than a visible one.
func (c *Conn) OnEvent(handler func(Event)) (cancel func()) {
	c.mu.Lock()
	id := c.nextHnd
	c.nextHnd++
	c.handlers[id] = handler
	c.mu.Unlock()
	return func() {
		c.mu.Lock()
		delete(c.handlers, id)
		c.mu.Unlock()
	}
}

// Done closes when the connection ends.
func (c *Conn) Done() <-chan struct{} { return c.done }

// Close ends the connection.
func (c *Conn) Close() error {
	c.shutdown(nil)
	return nil
}
