package chrome

import (
	"context"
	"testing"
	"time"
)

func settling(within, overall time.Duration) (*Driver, *session) {
	driver := &Driver{readyWithin: overall, settleWithin: within}
	return driver, newTestSession()
}

func (d *Driver) setCarrying(entry *session, n int) {
	d.mu.Lock()
	entry.carrying = n
	d.mu.Unlock()
}

// TestAnActThatStartsNothingIsNotWaitedOn.
//
// The wait after a non-navigating act is a REACTION window, and it has to stay one. Most acts start
// nothing — a scroll into view, a click that opens a menu — and paying a real wait on each of them
// would turn the cheapest verb in the set into the most expensive, which is how a correctness fix
// becomes the reason nobody uses the tool.
func TestAnActThatStartsNothingIsNotWaitedOn(t *testing.T) {
	driver, entry := settling(50*time.Millisecond, 5*time.Second)

	began := time.Now()
	if driver.awaitSettled(context.Background(), entry) {
		t.Fatal("a page that was asked for nothing is not still loading")
	}
	if spent := time.Since(began); spent > time.Second {
		t.Fatalf("waited %v for something that never started", spent)
	}
}

// TestAnActThatFetchesIsWaitedOnUntilTheAnswerIsIn.
//
// This is the load race one layer in, and it was reopened by the thing that closed it. On a page
// that renders itself from an API the ordinary interaction is a click, and a click does not
// navigate — so the act returned the moment the CDP call came back, which is before the answer it
// had just asked for existed. Open was covered, goto and back were covered, and the single most
// common case on the single class of page the ferry was built for was not.
func TestAnActThatFetchesIsWaitedOnUntilTheAnswerIsIn(t *testing.T) {
	driver, entry := settling(50*time.Millisecond, 5*time.Second)
	driver.setCarrying(entry, 1)

	arrives := 250 * time.Millisecond
	go func() {
		time.Sleep(arrives)
		driver.setCarrying(entry, 0)
	}()

	began := time.Now()
	if driver.awaitSettled(context.Background(), entry) {
		t.Fatal("the answer arrived, so the page is not still loading")
	}
	if spent := time.Since(began); spent < arrives {
		t.Fatalf("returned after %v, and the request it started was still in flight until %v", spent, arrives)
	}
}

// TestAPageThatNeverStopsAskingIsSaidToBeUnfinished.
//
// The bound is the same one Open uses, and reaching it is REPORTED. A wait with no bound would hang
// the agent on a page that polls; a bound that passed silently would hand back a half-rendered page
// as a finished one, which is the failure this whole file exists to stop.
func TestAPageThatNeverStopsAskingIsSaidToBeUnfinished(t *testing.T) {
	driver, entry := settling(20*time.Millisecond, 150*time.Millisecond)
	driver.setCarrying(entry, 1)

	if !driver.awaitSettled(context.Background(), entry) {
		t.Fatal("the page never stopped asking and was reported as finished")
	}
}
