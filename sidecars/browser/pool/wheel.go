// §spec pilar-de-browser

package pool

import (
	"context"
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/profile"
)

// The handover, which is a process swap and not a mode change (spec §4.2).
//
// Chrome does not go from headless to headful while it runs. So the wheel changes hands by closing
// one browser — gracefully, because a kill loses the profile's last writes — and starting another
// over the same --user-data-dir. Everything that survives is what was in the profile: the cookies,
// the storage, and after the person logs in, the session the agent came back for.
//
// This is the only part of the sidecar that can do it, because it is the only part that holds both
// the launcher and the profile directories. A single browser cannot replace itself.

// TakeWheel closes what is running and opens a window a person can see.
func (p *Pool) TakeWheel(ctx context.Context, req browser.WheelRequest) (browser.Wheel, error) {
	if err := req.Placement.Profile.Validate(); err != nil {
		return browser.Wheel{}, err
	}
	// Spec §4.5. A handover into a throwaway would ask for a login that is deleted with the run, and
	// the proposal that outlives the run would point at a profile that no longer exists. The núcleo
	// re-places the request onto the project's profile before it gets here; this refuses rather than
	// trusts, because the cost of being wrong is a person's time spent on nothing.
	if req.Placement.Profile.Kind != profile.Project {
		return browser.Wheel{}, fmt.Errorf("%w: %s", browser.ErrNotAProjectProfile, req.Placement.Profile.Kind)
	}

	// The agent's own session goes first, and its failure does not stop anything: it is usually in a
	// DIFFERENT profile — the throwaway the run was browsing in — and closing it there releases that
	// browser and deletes that directory. By the time the person's window opens, the run's browser is
	// gone rather than idling behind it.
	if req.Session != "" {
		if session, err := p.lookup(req.Session); err == nil {
			_ = session.holder.driver.Close(ctx, session.inner)
			p.release(ctx, session.holder, req.Session)
		}
	}

	holder, displaced := p.beginHuman(ctx, req.Placement.Profile)

	if err := p.reserve(); err != nil {
		p.abandon(ctx, holder)
		return browser.Wheel{}, err
	}
	defer p.unreserve()

	p.startHuman(ctx, holder)
	<-holder.ready
	if holder.err != nil {
		// Spec §4.4a: the wheel does not go back to the agent because our launch failed. The núcleo
		// records the session as a failed delivery and leaves the proposal for the person to retry or
		// abandon; nothing here decides that, and nothing here silently reopens headless.
		return browser.Wheel{}, holder.err
	}

	session, err := holder.driver.Open(ctx, browser.OpenRequest{
		URL:       req.URL,
		Placement: req.Placement,
	})
	if err != nil {
		p.release(ctx, holder, "")
		return browser.Wheel{}, err
	}

	p.mu.Lock()
	p.counter++
	outer := browser.SessionID(fmt.Sprintf("h%d", p.counter))
	holder.sessions[outer] = struct{}{}
	p.sessions[outer] = placed{holder: holder, inner: session.ID}
	p.mu.Unlock()

	return browser.Wheel{
		Session:   outer,
		Mode:      browser.ModeHuman,
		URL:       session.FinalURL,
		Displaced: displaced,
	}, nil
}

// ReturnWheel closes the person's window and reports where they went.
func (p *Pool) ReturnWheel(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.Returned{}, err
	}
	if !session.holder.human {
		return browser.Returned{}, fmt.Errorf("%w: %s", browser.ErrNoWheelToReturn, id)
	}

	// Read BEFORE the shutdown. The recorder lives in the driver, and the driver is about to be
	// stopped along with the browser it speaks to.
	chain := chainOf(session.holder.driver)

	_ = session.holder.driver.Close(ctx, session.inner)
	// release takes the browser down gracefully because this was its only session — which is the step
	// that flushes the login to disk (spec §4.2). The profile itself stays: Store.Discard refuses to
	// delete a project profile, which is the whole reason the handover targets one.
	p.release(ctx, session.holder, id)
	return browser.Returned{Chain: chain}, nil
}

// Forget deletes a profile and everything in it — spec §10's "Esquecer".
//
// The browser goes first and the directory second, and on Windows that order is not a preference: a
// running Chrome holds files under its --user-data-dir open, and RemoveAll over them fails halfway,
// leaving a profile that is neither there nor gone.
//
// It reports which sessions it took down for the same reason TakeWheel does: the núcleo has rows for
// them, and rows saying "open" about a browser that has been stopped are rows the UI acts on.
func (p *Pool) Forget(ctx context.Context, ref profile.Ref) ([]browser.SessionID, error) {
	if err := ref.Validate(); err != nil {
		return nil, err
	}

	p.mu.Lock()
	holder := p.running[ref]
	var stopped []browser.SessionID
	if holder != nil {
		for id := range holder.sessions {
			stopped = append(stopped, id)
			delete(p.sessions, id)
		}
		delete(p.running, ref)
	}
	p.mu.Unlock()

	if holder != nil {
		<-holder.ready
		if holder.driver != nil {
			holder.driver.Shutdown(ctx)
		}
	}
	return stopped, p.store.Forget(ref)
}

// beginHuman publishes the person's entry and evicts whatever held the profile, in one step.
//
// One step because spec §4.1 allows one browser per profile and no gap: if the eviction and the
// publication were two critical sections, a concurrent Open would find the profile free and start a
// headless browser over the directory the headful one is about to claim — and the second of the two
// silently inherits the first (launch.ErrInheritedInstance).
func (p *Pool) beginHuman(ctx context.Context, ref profile.Ref) (*entry, []browser.SessionID) {
	p.mu.Lock()
	previous := p.running[ref]
	var displaced []browser.SessionID
	if previous != nil {
		for id := range previous.sessions {
			displaced = append(displaced, id)
			delete(p.sessions, id)
		}
		delete(p.running, ref)
	}
	holder := &entry{
		ref:      ref,
		human:    true,
		ready:    make(chan struct{}),
		sessions: map[browser.SessionID]struct{}{},
	}
	p.running[ref] = holder
	p.mu.Unlock()

	if previous != nil {
		<-previous.ready
		if previous.driver != nil {
			previous.driver.Shutdown(ctx)
		}
	}
	return holder, displaced
}

// startHuman launches the headful browser into an entry that is already published.
func (p *Pool) startHuman(ctx context.Context, holder *entry) {
	defer close(holder.ready)

	fail := func(err error) {
		holder.err = err
		p.mu.Lock()
		if p.running[holder.ref] == holder {
			delete(p.running, holder.ref)
		}
		p.mu.Unlock()
	}

	if err := p.store.Admit(holder.ref, p.limits); err != nil {
		fail(err)
		return
	}
	dir, err := p.store.Prepare(holder.ref)
	if err != nil {
		fail(err)
		return
	}
	driver, err := p.launcher.LaunchHuman(ctx, dir)
	if err != nil {
		fail(err)
		return
	}
	holder.driver = driver
}

// abandon drops an entry that was published but never launched — the ceiling refused it.
func (p *Pool) abandon(ctx context.Context, holder *entry) {
	p.mu.Lock()
	if p.running[holder.ref] == holder {
		delete(p.running, holder.ref)
	}
	p.mu.Unlock()
	close(holder.ready)
	_ = ctx
}

// chainOf reads the navigation a driver recorded, when it is the kind that records one.
//
// An interface assertion rather than a method on Instance, so that a driver which cannot record —
// every agent-mode one — is not obliged to answer a question that is not its. What comes back for
// those is an empty chain, which grants nothing, which is the safe direction.
func chainOf(driver Instance) []string {
	recorder, ok := driver.(interface{ Chain() []string })
	if !ok {
		return nil
	}
	return recorder.Chain()
}
