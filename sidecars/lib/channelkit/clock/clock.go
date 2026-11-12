// Package clock is the injectable clock behind every wait in the kit (lease,
// renewal, backoff, due_at, long-poll). Production code takes a Clock; tests
// pass a Fake and advance it, so no test sleeps in real time.
package clock

import "time"

// Timer is a single-shot timer, the subset of *time.Timer the kit uses.
type Timer interface {
	// C is the channel the firing time is delivered on. It is nil for
	// timers made by Fake.AfterFunc, which run a callback instead.
	C() <-chan time.Time
	// Stop prevents the timer from firing. It reports whether the timer
	// was still pending.
	Stop() bool
	// Reset re-arms the timer to fire after d. It reports whether the
	// timer was still pending before the call.
	Reset(d time.Duration) bool
}

// Clock is the time source consumers depend on.
type Clock interface {
	Now() time.Time
	NewTimer(d time.Duration) Timer
	After(d time.Duration) <-chan time.Time
	Since(t time.Time) time.Duration
}

type realClock struct{}

// Real returns the Clock backed by the standard library.
func Real() Clock { return realClock{} }

func (realClock) Now() time.Time                         { return time.Now() }
func (realClock) After(d time.Duration) <-chan time.Time { return time.After(d) }
func (realClock) Since(t time.Time) time.Duration        { return time.Since(t) }
func (realClock) NewTimer(d time.Duration) Timer         { return realTimer{t: time.NewTimer(d)} }

type realTimer struct{ t *time.Timer }

func (r realTimer) C() <-chan time.Time        { return r.t.C }
func (r realTimer) Stop() bool                 { return r.t.Stop() }
func (r realTimer) Reset(d time.Duration) bool { return r.t.Reset(d) }
