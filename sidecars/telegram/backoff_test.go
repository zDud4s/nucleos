package main

import (
	"testing"
	"time"
)

func TestNextRetryDelay(t *testing.T) {
	d := updatesRetryBase
	want := []time.Duration{4, 8, 16, 32, 60, 60}
	for i, w := range want {
		d = nextRetryDelay(d, updatesRetryBase, updatesRetryMax)
		if d != w*time.Second {
			t.Errorf("step %d: got %v, want %v", i, d, w*time.Second)
		}
	}
}
