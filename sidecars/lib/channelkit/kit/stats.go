package kit

import "sync/atomic"

// Stats holds the runtime counters of a channel adapter.
type Stats struct {
	PermanentFailures  atomic.Int64
	RedeliveredSkipped atomic.Int64
	LateAcks           atomic.Int64
	AckRejected        atomic.Int64
	Executions         atomic.Int64
	Panics             atomic.Int64
}

// StatsSnapshot is a plain copy of the counters at one point in time.
type StatsSnapshot struct {
	PermanentFailures  int64
	RedeliveredSkipped int64
	LateAcks           int64
	AckRejected        int64
	Executions         int64
	Panics             int64
}

// Snapshot reads every counter. Each field is read atomically; the set is not
// a single atomic transaction.
func (s *Stats) Snapshot() StatsSnapshot {
	return StatsSnapshot{
		PermanentFailures:  s.PermanentFailures.Load(),
		RedeliveredSkipped: s.RedeliveredSkipped.Load(),
		LateAcks:           s.LateAcks.Load(),
		AckRejected:        s.AckRejected.Load(),
		Executions:         s.Executions.Load(),
		Panics:             s.Panics.Load(),
	}
}
