package serve

import (
	"sync"
	"time"
)

const limiterSetCap = 1024

type limiterBucket struct {
	events  []time.Time
	limited bool
}

// limiterSet keeps bounded, lazy per-device sliding windows.
type limiterSet struct {
	mu     sync.Mutex
	limit  int
	window time.Duration
	now    func() time.Time
	m      map[int64]*limiterBucket
	// lastSweep is when the idle buckets were last dropped.
	lastSweep time.Time
}

func newLimiterSet(limit int, window time.Duration) *limiterSet {
	return &limiterSet{limit: limit, window: window, now: time.Now, m: make(map[int64]*limiterBucket)}
}

func (s *limiterSet) prune(b *limiterBucket, now time.Time) {
	kept := b.events[:0]
	for _, event := range b.events {
		if now.Sub(event) < s.window {
			kept = append(kept, event)
		}
	}
	b.events = kept
}

func (s *limiterSet) sweepLocked(now time.Time) {
	s.lastSweep = now
	for id, b := range s.m {
		s.prune(b, now)
		if len(b.events) == 0 {
			delete(s.m, id)
		}
	}
}

// maybeSweepLocked keeps the O(n) scan off the hot path: idle buckets are dropped at most once per
// quarter window. Every access prunes the bucket it touches, so skipping a sweep never changes a verdict.
func (s *limiterSet) maybeSweepLocked(now time.Time) {
	if now.Sub(s.lastSweep) >= s.window/4 {
		s.sweepLocked(now)
	}
}

// allow records an event. At capacity a previously unseen device falls through to the global limit;
// the failure brake (pfail) is a limiterSet too, so a device past the cap has no brake either, which
// is acceptable because the global ceiling still bounds the flood.
func (s *limiterSet) allow(id int64) (bool, time.Duration, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.now()
	s.maybeSweepLocked(now)
	b := s.m[id]
	if b == nil {
		if len(s.m) >= limiterSetCap && now.Sub(s.lastSweep) >= time.Second {
			s.sweepLocked(now)
		}
		if len(s.m) >= limiterSetCap {
			return true, 0, false
		}
		b = &limiterBucket{}
		s.m[id] = b
	}
	s.prune(b, now)
	if len(b.events) >= s.limit {
		transitioned := !b.limited
		b.limited = true
		return false, s.window - now.Sub(b.events[0]), transitioned
	}
	b.limited = false
	b.events = append(b.events, now)
	return true, 0, false
}

// blocked checks an existing bucket without recording an event.
func (s *limiterSet) blocked(id int64) (bool, time.Duration, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.now()
	s.maybeSweepLocked(now)
	b := s.m[id]
	if b == nil {
		return false, 0, false
	}
	s.prune(b, now)
	if len(b.events) < s.limit {
		b.limited = false
		return false, 0, false
	}
	transitioned := !b.limited
	b.limited = true
	return true, s.window - now.Sub(b.events[0]), transitioned
}

func (s *limiterSet) reset(id int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.m, id)
}
