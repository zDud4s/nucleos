package clock

import (
	"sync"
	"testing"
	"time"
)

var t0 = time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

func fired(ch <-chan time.Time) bool {
	select {
	case <-ch:
		return true
	default:
		return false
	}
}

func TestFakeTimerFiresOnlyOnAdvance(t *testing.T) {
	f := NewFake(t0)
	tm := f.NewTimer(10 * time.Second)
	if fired(tm.C()) {
		t.Fatal("timer fired before any advance")
	}
	f.Advance(9 * time.Second)
	if fired(tm.C()) {
		t.Fatal("timer fired before its deadline")
	}
	f.Advance(time.Second)
	if !fired(tm.C()) {
		t.Fatal("timer did not fire at its deadline")
	}
	if got := f.Since(t0); got != 10*time.Second {
		t.Fatalf("Since = %v, want 10s", got)
	}
}

func TestFakeTimersFireInDeadlineOrder(t *testing.T) {
	f := NewFake(t0)
	var order []int
	for i, d := range []time.Duration{30, 10, 20} {
		i := i
		f.AfterFunc(d*time.Second, func() { order = append(order, i) })
	}
	f.Advance(time.Minute)
	want := []int{1, 2, 0}
	if len(order) != 3 || order[0] != want[0] || order[1] != want[1] || order[2] != want[2] {
		t.Fatalf("order = %v, want %v", order, want)
	}
}

func TestFakeStopBeforeDeadlineDoesNotFire(t *testing.T) {
	f := NewFake(t0)
	tm := f.NewTimer(10 * time.Second)
	if !tm.Stop() {
		t.Fatal("Stop on an active timer must report true")
	}
	if tm.Stop() {
		t.Fatal("second Stop must report false")
	}
	f.Advance(time.Minute)
	if fired(tm.C()) {
		t.Fatal("stopped timer fired")
	}
	if f.Pending() != 0 {
		t.Fatalf("Pending = %d, want 0", f.Pending())
	}
}

func TestFakeResetRearms(t *testing.T) {
	f := NewFake(t0)
	tm := f.NewTimer(10 * time.Second)
	if !tm.Reset(20 * time.Second) {
		t.Fatal("Reset on an active timer must report true")
	}
	f.Advance(10 * time.Second)
	if fired(tm.C()) {
		t.Fatal("timer fired at its old deadline")
	}
	f.Advance(10 * time.Second)
	if !fired(tm.C()) {
		t.Fatal("timer did not fire at its new deadline")
	}
	if tm.Reset(5 * time.Second) {
		t.Fatal("Reset on a fired timer must report false")
	}
	if f.Pending() != 1 {
		t.Fatalf("Pending = %d, want 1", f.Pending())
	}
}

func TestFakeAfter(t *testing.T) {
	f := NewFake(t0)
	ch := f.After(5 * time.Second)
	f.Advance(5 * time.Second)
	got := <-ch
	if !got.Equal(t0.Add(5 * time.Second)) {
		t.Fatalf("After delivered %v", got)
	}
}

func TestFakeBlockUntilUnblocksWhenGoroutineArms(t *testing.T) {
	f := NewFake(t0)
	done := make(chan struct{})
	go func() {
		defer close(done)
		<-f.After(time.Second)
	}()
	f.BlockUntil(1)
	f.Advance(time.Second)
	<-done
}

func TestFakePendingCounts(t *testing.T) {
	f := NewFake(t0)
	if f.Pending() != 0 {
		t.Fatal("new fake must have no pending timers")
	}
	a := f.NewTimer(time.Second)
	f.NewTimer(2 * time.Second)
	f.NewTimer(3 * time.Second)
	if f.Pending() != 3 {
		t.Fatalf("Pending = %d, want 3", f.Pending())
	}
	a.Stop()
	if f.Pending() != 2 {
		t.Fatalf("Pending = %d, want 2", f.Pending())
	}
	f.Advance(2 * time.Second)
	if f.Pending() != 1 {
		t.Fatalf("Pending = %d, want 1", f.Pending())
	}
}

func TestFakeRearmWithinAdvanceFiresInWindow(t *testing.T) {
	f := NewFake(t0)
	var stamps []time.Duration
	var tm Timer
	tm = f.AfterFunc(30*time.Second, func() {
		stamps = append(stamps, f.Since(t0))
		tm.Reset(30 * time.Second)
	})
	f.Advance(100 * time.Second)
	if len(stamps) != 3 || stamps[0] != 30*time.Second || stamps[1] != 60*time.Second || stamps[2] != 90*time.Second {
		t.Fatalf("fires at %v, want 30s/60s/90s", stamps)
	}
	if f.Pending() != 1 {
		t.Fatalf("Pending = %d, want 1 (the 120s re-arm)", f.Pending())
	}
	if got := f.Since(t0); got != 100*time.Second {
		t.Fatalf("Since = %v, want 100s", got)
	}
}

func TestFakeConcurrentAdvanceIsRaceFree(t *testing.T) {
	f := NewFake(t0)
	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(2)
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				f.Advance(time.Second)
			}
		}()
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				tm := f.NewTimer(time.Second)
				_ = f.Pending()
				_ = f.Now()
				tm.Stop()
			}
		}()
	}
	wg.Wait()
	if got := f.Since(t0); got != 400*time.Second {
		t.Fatalf("Since = %v, want 400s", got)
	}
}

func TestRealClock(t *testing.T) {
	c := Real()
	start := c.Now()
	tm := c.NewTimer(time.Millisecond)
	<-tm.C()
	if c.Since(start) <= 0 {
		t.Fatal("Since must be positive after a timer fired")
	}
	if tm.Stop() {
		t.Fatal("Stop after firing must report false")
	}
	<-c.After(time.Millisecond)
}
