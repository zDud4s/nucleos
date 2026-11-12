package clock

import (
	"sync"
	"time"
)

// Fake is a manually driven Clock. Time moves only through Advance.
//
// Advance rule: timers fire in deadline order (ties by arming order), and
// Now() reads the timer's own deadline while it fires. A timer re-armed
// during the same Advance step (typically from an AfterFunc callback, which
// runs synchronously inside Advance) fires again within that step if its new
// deadline is at or before the window end (Now+d); otherwise it stays pending.
//
// Channel timers deliver into a buffered channel without blocking, like the
// standard library. Advance does not wait for goroutines that consume them:
// a test that needs a consumer to have re-armed calls BlockUntil before the
// next Advance.
type Fake struct {
	mu      sync.Mutex
	cond    *sync.Cond
	now     time.Time
	seq     uint64
	pending []*fakeTimer
}

type fakeTimer struct {
	f        *Fake
	ch       chan time.Time
	fn       func()
	deadline time.Time
	seq      uint64
	active   bool
}

// NewFake returns a Fake whose clock starts at start.
func NewFake(start time.Time) *Fake {
	f := &Fake{now: start}
	f.cond = sync.NewCond(&f.mu)
	return f
}

// Now returns the fake time.
func (f *Fake) Now() time.Time {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.now
}

// Since returns the fake time elapsed since t.
func (f *Fake) Since(t time.Time) time.Duration { return f.Now().Sub(t) }

// NewTimer arms a channel timer d from the fake now.
func (f *Fake) NewTimer(d time.Duration) Timer {
	t := &fakeTimer{f: f, ch: make(chan time.Time, 1)}
	f.mu.Lock()
	f.arm(t, d)
	f.mu.Unlock()
	return t
}

// After is NewTimer(d).C().
func (f *Fake) After(d time.Duration) <-chan time.Time { return f.NewTimer(d).C() }

// AfterFunc arms a timer that runs fn synchronously inside Advance when it
// fires. fn may call Reset on the returned timer or arm others, but must not
// call Advance.
func (f *Fake) AfterFunc(d time.Duration, fn func()) Timer {
	t := &fakeTimer{f: f, fn: fn}
	f.mu.Lock()
	f.arm(t, d)
	f.mu.Unlock()
	return t
}

// Pending returns the number of armed timers that have not fired.
func (f *Fake) Pending() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.pending)
}

// BlockUntil blocks until at least n timers are pending.
func (f *Fake) BlockUntil(n int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	for len(f.pending) < n {
		f.cond.Wait()
	}
}

// Advance moves the clock forward by d, firing due timers in deadline order.
func (f *Fake) Advance(d time.Duration) {
	f.mu.Lock()
	target := f.now.Add(d)
	for {
		next := f.earliest()
		if next == nil || next.deadline.After(target) {
			break
		}
		f.remove(next)
		if next.deadline.After(f.now) {
			f.now = next.deadline
		}
		fireAt, fn := f.now, next.fn
		if fn == nil {
			select {
			case next.ch <- fireAt:
			default:
			}
		}
		f.cond.Broadcast()
		if fn != nil {
			f.mu.Unlock()
			fn()
			f.mu.Lock()
		}
	}
	if target.After(f.now) {
		f.now = target
	}
	f.mu.Unlock()
}

// arm schedules t. The caller holds f.mu.
func (f *Fake) arm(t *fakeTimer, d time.Duration) {
	f.seq++
	t.seq = f.seq
	t.deadline = f.now.Add(d)
	t.active = true
	f.pending = append(f.pending, t)
	f.cond.Broadcast()
}

// remove deactivates t. The caller holds f.mu.
func (f *Fake) remove(t *fakeTimer) {
	t.active = false
	for i, p := range f.pending {
		if p == t {
			f.pending = append(f.pending[:i], f.pending[i+1:]...)
			break
		}
	}
}

// earliest returns the pending timer with the smallest (deadline, seq).
func (f *Fake) earliest() *fakeTimer {
	var best *fakeTimer
	for _, p := range f.pending {
		if best == nil || p.deadline.Before(best.deadline) ||
			(p.deadline.Equal(best.deadline) && p.seq < best.seq) {
			best = p
		}
	}
	return best
}

func (t *fakeTimer) C() <-chan time.Time {
	if t.ch == nil {
		return nil
	}
	return t.ch
}

func (t *fakeTimer) Stop() bool {
	t.f.mu.Lock()
	defer t.f.mu.Unlock()
	was := t.active
	if was {
		t.f.remove(t)
		t.f.cond.Broadcast()
	}
	return was
}

func (t *fakeTimer) Reset(d time.Duration) bool {
	t.f.mu.Lock()
	defer t.f.mu.Unlock()
	was := t.active
	if was {
		t.f.remove(t)
	}
	t.f.arm(t, d)
	return was
}
