// Package kit holds the runtime pieces shared by every channel adapter.
package kit

import "sync"

// Inflight tracks the ids of events currently being processed. It lives in
// memory only and is never persisted.
type Inflight struct {
	mu  sync.Mutex
	ids map[string]struct{}
}

// NewInflight returns an empty in-flight set.
func NewInflight() *Inflight {
	return &Inflight{ids: make(map[string]struct{})}
}

// TryAdd reports true the first time id is added, and false until Done(id).
func (f *Inflight) TryAdd(id string) bool {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.ids == nil {
		f.ids = make(map[string]struct{})
	}
	if _, ok := f.ids[id]; ok {
		return false
	}
	f.ids[id] = struct{}{}
	return true
}

// Done releases id. An unknown id is a no-op.
func (f *Inflight) Done(id string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.ids, id)
}

// Len returns the number of ids currently in flight.
func (f *Inflight) Len() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.ids)
}
