package chrome

import (
	"context"
	"testing"
	"time"
)

func settling(within, overall time.Duration) (*Driver, *session) {
	driver := &Driver{readyWithin: overall, settleWithin: within, movingWithin: overall}
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
	if driver.awaitSettled(context.Background(), entry, began) {
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
	if driver.awaitSettled(context.Background(), entry, began) {
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

	if !driver.awaitSettled(context.Background(), entry, time.Now()) {
		t.Fatal("the page never stopped asking and was reported as finished")
	}
}

func (d *Driver) setChanged(entry *session, at time.Time) {
	d.mu.Lock()
	entry.changedAt = at
	d.mu.Unlock()
}

// TestAnActThatRedrawsIsWaitedOnEvenThoughItAskedForNothing.
//
// The half the ferry could not see. Plenty of acts change a page without any request at all — a menu
// that opens, a route that renders from data already in memory, a list that filters itself — and for
// those the wait had nothing to watch and returned before the page had drawn. The page's own
// MutationObserver is the signal; this is the wait learning to use it.
func TestAnActThatRedrawsIsWaitedOnEvenThoughItAskedForNothing(t *testing.T) {
	// The real window here, not a short one: what is being asserted is that a redraw arriving inside
	// it is waited for, and shortening the window would assert that against a bound nobody ships.
	driver, entry := settling(settleGrace, 5*time.Second)
	driver.movingWithin = 5 * time.Second

	draws := 150 * time.Millisecond
	since := time.Now()
	go func() {
		time.Sleep(draws)
		driver.setChanged(entry, time.Now())
	}()

	if driver.awaitSettled(context.Background(), entry, since) {
		t.Fatal("the page drew and went quiet, so it is not still loading")
	}
	if spent := time.Since(since); spent < draws {
		t.Fatalf("returned after %v, and the page had not drawn until %v", spent, draws)
	}
}

// TestARedrawFromBeforeTheActIsNotEvidenceTheActDidAnything.
//
// A page that was already moving would otherwise make every act on it look like an act that started
// something, and the wait would be spent on somebody else's news.
func TestARedrawFromBeforeTheActIsNotEvidenceTheActDidAnything(t *testing.T) {
	driver, entry := settling(50*time.Millisecond, 5*time.Second)
	driver.movingWithin = 5 * time.Second
	driver.setChanged(entry, time.Now())

	since := time.Now().Add(time.Millisecond)
	began := time.Now()
	if driver.awaitSettled(context.Background(), entry, since) {
		t.Fatal("nothing was in flight")
	}
	if spent := time.Since(began); spent > time.Second {
		t.Fatalf("waited %v on a redraw that happened before the act", spent)
	}
}

// TestAPageThatNeverStopsRedrawingIsOrdinary.
//
// A clock, a carousel, a spinner. Waiting one out would make every act on such a page cost the full
// load deadline, and reporting it as unfinished would be worse still — the document is there and the
// agent can read it. So the wait is capped and the cap is NOT a complaint.
func TestAPageThatNeverStopsRedrawingIsOrdinary(t *testing.T) {
	driver, entry := settling(50*time.Millisecond, 10*time.Second)
	driver.movingWithin = 300 * time.Millisecond

	stop := make(chan struct{})
	defer close(stop)
	go func() {
		for {
			select {
			case <-stop:
				return
			case <-time.After(20 * time.Millisecond):
				driver.setChanged(entry, time.Now())
			}
		}
	}()

	began := time.Now()
	if driver.awaitSettled(context.Background(), entry, began) {
		t.Fatal("a page with something ticking on it was reported as unfinished")
	}
	if spent := time.Since(began); spent > 3*time.Second {
		t.Fatalf("a page that never settles held the act for %v", spent)
	}
}

// TestARedrawAfterTheReactionWindowIsNotWaitedFor.
//
// The bound stated as a test rather than left in a comment. A page that begins to respond a whole
// window after the click is read as it stood, and the honest part is what happens next: the reading
// says `still_loading`, and another one is cheap. A window wide enough to catch every late render
// would be a window every inert act pays for.
func TestARedrawAfterTheReactionWindowIsNotWaitedFor(t *testing.T) {
	driver, entry := settling(60*time.Millisecond, 5*time.Second)
	driver.movingWithin = 5 * time.Second

	since := time.Now()
	go func() {
		time.Sleep(400 * time.Millisecond)
		driver.setChanged(entry, time.Now())
	}()

	if driver.awaitSettled(context.Background(), entry, since) {
		t.Fatal("nothing was in flight")
	}
	if spent := time.Since(since); spent > 300*time.Millisecond {
		t.Fatalf("waited %v for a redraw that arrived long after the act", spent)
	}
}

// TestASecondRedrawInsideTheQuietWindowExtendsTheWait.
//
// A page that draws a placeholder and then its content is drawing twice, and the second half arrives
// from a timer nothing on this side can see. Silence is the only evidence there is, so the rule is
// how long silence has to last before it counts as finished — and both halves of that rule are
// asserted, here and below, because a bound stated only in a comment is a bound nobody keeps.
func TestASecondRedrawInsideTheQuietWindowExtendsTheWait(t *testing.T) {
	driver, entry := settling(settleGrace, 10*time.Second)
	driver.movingWithin = 10 * time.Second

	since := time.Now()
	go func() {
		time.Sleep(100 * time.Millisecond)
		driver.setChanged(entry, time.Now())
		time.Sleep(quietAfterChange - 200*time.Millisecond)
		driver.setChanged(entry, time.Now())
	}()

	if driver.awaitSettled(context.Background(), entry, since) {
		t.Fatal("the page drew twice and stopped; it is not still loading")
	}
	// Past the second draw, which is the whole claim: a wait that ended on the first would have
	// returned with the placeholder on screen.
	if spent := time.Since(since); spent < 100*time.Millisecond+quietAfterChange-200*time.Millisecond {
		t.Fatalf("returned after %v, before the second draw", spent)
	}
}

// TestASecondRedrawAfterTheQuietWindowIsNotWaitedFor.
//
// The other half, and the honest one. There is no signal for a timer, so a page whose two halves are
// further apart than the window is read as it stood after the first — and the cost of covering it
// would be paid by every act that redraws once and stops.
func TestASecondRedrawAfterTheQuietWindowIsNotWaitedFor(t *testing.T) {
	driver, entry := settling(settleGrace, 10*time.Second)
	driver.movingWithin = 10 * time.Second

	since := time.Now()
	driver.setChanged(entry, since)
	go func() {
		time.Sleep(quietAfterChange + 400*time.Millisecond)
		driver.setChanged(entry, time.Now())
	}()

	if driver.awaitSettled(context.Background(), entry, since) {
		t.Fatal("nothing was in flight")
	}
	if spent := time.Since(since); spent > quietAfterChange+300*time.Millisecond {
		t.Fatalf("waited %v, so the window is not the bound it says it is", spent)
	}
}
