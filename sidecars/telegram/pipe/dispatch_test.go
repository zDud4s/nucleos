package pipe

import (
	"testing"
	"time"
)

// The update loop is the only channel that carries `/kill`. Handling ran inside it, so one slow
// update — a download, a transcription, a daemon that stopped answering — stalled every update
// behind it while the notifier kept announcing events: a bot that looks alive with a dead command
// path.
func TestASlowUpdateDoesNotStallTheChannelBehindIt(t *testing.T) {
	dispatcher := NewDispatcher()
	stuck := make(chan struct{})
	defer close(stuck)

	dispatcher.Dispatch(1, func() { <-stuck })

	done := make(chan struct{})
	dispatcher.Dispatch(2, func() { close(done) })

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("the second chat never ran while the first was stuck")
	}
}

// Serialised per chat, though: the núcleo gives a chat one turn at a time (its ChatSlot), so two
// updates from the same chat running at once would race for a slot one of them cannot get.
func TestUpdatesFromOneChatRunOneAtATimeAndInOrder(t *testing.T) {
	dispatcher := NewDispatcher()
	const updates = 25

	order := make(chan int, updates)
	slot := make(chan struct{}, 1)
	for i := 0; i < updates; i++ {
		dispatcher.Dispatch(7, func() {
			select {
			case slot <- struct{}{}:
			default:
				t.Error("two updates from the same chat ran at the same time")
				return
			}
			order <- i
			<-slot
		})
	}
	dispatcher.Shutdown(5 * time.Second)

	close(order)
	want := 0
	for got := range order {
		if got != want {
			t.Fatalf("update %d ran in position %d, want them in arrival order", got, want)
		}
		want++
	}
	if want != updates {
		t.Errorf("updates run = %d, want %d", want, updates)
	}
}

// A panic in one update used to take the process down with it, and with it the only way to stop an
// autonomous agent. The update is lost; the channel is not.
func TestAPanickingUpdateDoesNotTakeTheChannelDown(t *testing.T) {
	dispatcher := NewDispatcher()
	survived := make(chan struct{})

	dispatcher.Dispatch(1, func() { panic("a nil map, somewhere deep") })
	dispatcher.Dispatch(1, func() { close(survived) })

	select {
	case <-survived:
	case <-time.After(5 * time.Second):
		t.Fatal("the update after the panic never ran")
	}
	dispatcher.Shutdown(5 * time.Second)
}

// Shutdown is what makes a restart safe: an approval already being acted on finishes rather than
// disappearing halfway.
func TestShutdownWaitsForWorkAlreadyInFlight(t *testing.T) {
	dispatcher := NewDispatcher()
	finished := make(chan struct{})

	dispatcher.Dispatch(1, func() {
		time.Sleep(50 * time.Millisecond)
		close(finished)
	})
	dispatcher.Shutdown(5 * time.Second)

	select {
	case <-finished:
	default:
		t.Error("Shutdown() returned while an update was still running")
	}
}

func TestRunGuardedReportsWhetherTheWorkFinished(t *testing.T) {
	if !runGuarded("test", func() {}) {
		t.Error("runGuarded(clean work) = false, want true")
	}
	if runGuarded("test", func() { panic("boom") }) {
		t.Error("runGuarded(panicking work) = true, want false")
	}
}
