package kit

import (
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
)

func TestInflightTryAddOnce(t *testing.T) {
	f := NewInflight()
	if !f.TryAdd("a") {
		t.Fatal("first TryAdd must succeed")
	}
	if f.TryAdd("a") {
		t.Fatal("second TryAdd must fail")
	}
}

func TestInflightDoneReleases(t *testing.T) {
	f := NewInflight()
	f.TryAdd("a")
	f.Done("a")
	if !f.TryAdd("a") {
		t.Fatal("TryAdd after Done must succeed")
	}
}

func TestInflightLen(t *testing.T) {
	f := NewInflight()
	f.TryAdd("a")
	f.TryAdd("b")
	f.TryAdd("a")
	if f.Len() != 2 {
		t.Fatalf("Len = %d, want 2", f.Len())
	}
	f.Done("a")
	if f.Len() != 1 {
		t.Fatalf("Len = %d, want 1", f.Len())
	}
}

func TestInflightConcurrentSameIDOneWinner(t *testing.T) {
	f := NewInflight()
	var wins atomic.Int64
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if f.TryAdd("same") {
				wins.Add(1)
			}
		}()
	}
	wg.Wait()
	if wins.Load() != 1 {
		t.Fatalf("winners = %d, want 1", wins.Load())
	}
}

func TestInflightConcurrentDistinctIDs(t *testing.T) {
	f := NewInflight()
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			id := strconv.Itoa(i)
			if !f.TryAdd(id) {
				t.Errorf("TryAdd(%s) failed", id)
			}
			f.Len()
		}(i)
	}
	wg.Wait()
	if f.Len() != 100 {
		t.Fatalf("Len = %d, want 100", f.Len())
	}
}

func TestInflightDoneUnknownNoPanic(t *testing.T) {
	f := NewInflight()
	f.Done("nope")
	var zero Inflight
	zero.Done("nope")
	if f.Len() != 0 {
		t.Fatal("Len must stay 0")
	}
}

func TestStatsConcurrentIncrementsSumExactly(t *testing.T) {
	var s Stats
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			s.PermanentFailures.Add(1)
			s.RedeliveredSkipped.Add(2)
			s.LateAcks.Add(3)
			s.AckRejected.Add(4)
			s.Executions.Add(5)
			s.Panics.Add(6)
		}()
	}
	wg.Wait()
	got := s.Snapshot()
	want := StatsSnapshot{100, 200, 300, 400, 500, 600}
	if got != want {
		t.Fatalf("snapshot = %+v, want %+v", got, want)
	}
}

func TestStatsSnapshotConsistent(t *testing.T) {
	var s Stats
	s.Executions.Add(7)
	s.Panics.Add(1)
	snap := s.Snapshot()
	s.Executions.Add(1)
	if snap.Executions != 7 || snap.Panics != 1 || snap.LateAcks != 0 {
		t.Fatalf("snapshot = %+v", snap)
	}
	if s.Snapshot().Executions != 8 {
		t.Fatal("later snapshot must see the new value")
	}
}
