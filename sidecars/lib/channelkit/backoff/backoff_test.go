package backoff

import (
	"context"
	"testing"
	"time"

	"nucleoschannelkit/clock"
)

func TestNextDoublesWithinBounds(t *testing.T) {
	d := Updates.Base
	want := []time.Duration{4, 8, 16, 32, 60, 60}
	for i, w := range want {
		d = Next(d, Updates.Base, Updates.Max)
		if d != w*time.Second {
			t.Errorf("step %d: got %v, want %v", i, d, w*time.Second)
		}
	}
}

func TestNextClampsBelowBase(t *testing.T) {
	if got := Next(0, 2*time.Second, time.Minute); got != 2*time.Second {
		t.Errorf("got %v, want the base", got)
	}
}

func TestPolicies(t *testing.T) {
	if Updates.Base != 2*time.Second || Updates.Max != 60*time.Second {
		t.Errorf("Updates = %+v", Updates)
	}
	if Outbox.Base != 2*time.Second || Outbox.Max != 5*time.Minute {
		t.Errorf("Outbox = %+v", Outbox)
	}
}

func TestSleepElapses(t *testing.T) {
	fk := clock.NewFake(time.Unix(0, 0))
	res := make(chan bool, 1)
	go func() { res <- Sleep(context.Background(), fk, 5*time.Second) }()
	fk.BlockUntil(1)
	fk.Advance(5 * time.Second)
	if !<-res {
		t.Fatal("Sleep = false, want true after the delay")
	}
}

func TestSleepCancelled(t *testing.T) {
	fk := clock.NewFake(time.Unix(0, 0))
	ctx, cancel := context.WithCancel(context.Background())
	res := make(chan bool, 1)
	go func() { res <- Sleep(ctx, fk, time.Hour) }()
	fk.BlockUntil(1)
	cancel()
	if <-res {
		t.Fatal("Sleep = true, want false when ctx ends first")
	}
	if fk.Pending() != 0 {
		t.Errorf("timer left pending: %d", fk.Pending())
	}
}
