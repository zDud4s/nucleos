// §spec pilar-de-browser

// Package pool keeps one browser per profile, and routes each session to the right one.
//
// It exists because the two halves of this sidecar disagree about what a "session" is. The núcleo
// asks for a session on a URL and expects the profile of spec §5.3 to be honoured; a chrome.Driver
// is already bound to a profile — its --user-data-dir was fixed on the command line and its fence
// policy at Connect. Something has to stand between them and decide which browser a request belongs
// to. That is this package, and it is the only thing here that knows more than one browser exists.
//
// # One browser per profile, and never two
//
// Two Chromes over the same --user-data-dir do not both run: the second hands off to the first and
// exits, so the caller ends up driving a browser it did not configure and whose fence it cannot
// vouch for (launch.ErrInheritedInstance). Every launch is therefore serialised per profile, and a
// second Open for a profile that is still starting waits for the first rather than racing it.
//
// # What it does not do
//
// It does not decide anything. Which profile, and what that profile admits, arrive in the request
// from the núcleo (spec §5.3, §6.1); the pool's judgement is limited to refusing a request it cannot
// carry out honestly — a profile it may not create, a site list that disagrees with the one a live
// browser was launched with, one session too many.
package pool

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

const (
	// discardWithin bounds how long an ephemeral profile's directory is chased. Generous against a
	// slow machine and free against a person, because nothing is waiting on it: release runs after
	// the caller already has its answer. What it must not do is wait for ever, and see discard for
	// why a directory held open for ever is a different fault wearing this one's clothes.
	discardWithin = 5 * time.Second
	// discardRetry is the gap between attempts. Short, because what it waits out is a handful of
	// processes finishing their exit, not an interval anybody chose.
	discardRetry = 100 * time.Millisecond
)

// Instance is one running browser: a driver, plus the ability to be shut down gracefully.
//
// Shutdown is separate from browser.Driver.Close — which ends a session — because spec §4.2 measured
// that the difference matters: with a graceful browser close everything in the profile survives, and
// with a kill a write from seconds earlier comes back empty. The profile is the identity, so losing
// its last write is losing a login.
type Instance interface {
	browser.Driver
	Shutdown(ctx context.Context)
}

// Launcher starts a browser over a prepared profile directory. The seam that keeps this package
// testable without Chrome — the same role search.Provider plays in the web sidecar.
//
// Two methods and not one with a mode argument, because the two are not the same function with a
// flag: one takes a fence policy and refuses to build a command line without a proxy, and the other
// takes none and must not have one (spec §6.4). A single Launch(mode) would have a policy parameter
// that is required half the time and ignored the other half, and "ignored" is how a fence goes
// missing without anybody deleting it.
type Launcher interface {
	Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error)
	// LaunchHuman starts a headful browser with no fence, for spec §4.2's handover.
	LaunchHuman(ctx context.Context, dir string) (Instance, error)
	Name() string
}

// ErrTooManySessions is spec §9.6's ceiling, and it lives in `browser` now — see the comment there
// for why moving it was a bug fix rather than tidying. Kept as an alias so a caller that already
// names it keeps compiling, and so this package still reads as the thing that enforces the ceiling.
var ErrTooManySessions = browser.ErrTooManySessions

// ErrPolicyChanged means the núcleo sent a site list that differs from the one the live browser for
// that profile was launched with.
//
// It is refused rather than applied. A fence policy is fixed at Connect and read by both the CDP
// interception and the proxy; swapping it under a browser with live pages would change what is
// allowed halfway through loading a document, which is the one moment the decision has to be stable
// (spec §5.4: the decision must precede the execution). Once the profile goes idle the pool
// relaunches on the new list by itself, and the case that reaches this error is narrow: a list that
// changed while a session on that profile was still open.
var ErrPolicyChanged = errors.New("pool: the site list changed while a session on this profile was open")

// Pool routes sessions to browsers. Its zero value is not usable; see New.
type Pool struct {
	launcher Launcher
	store    profile.Store
	limits   profile.Limits
	max      int

	mu       sync.Mutex
	running  map[profile.Ref]*entry
	sessions map[browser.SessionID]placed
	counter  int
	// pending counts opens that have been admitted but have not produced a session yet. Without it
	// the ceiling is checked against a number that is always out of date by exactly the duration of a
	// navigation, and two concurrent opens both pass a limit of one.
	pending int
}

// placed is one session: the browser it lives in, and the id THAT browser knows it by.
//
// The two ids are not decoration. A driver numbers its sessions from one, so the first session of
// every browser is called the same thing — and the moment a second profile is open, one map keyed by
// the driver's id has two sessions claiming one key. The pool routes, so the pool owns the namespace
// it routes in: callers get an id minted here, and each driver keeps hearing its own.
type placed struct {
	holder *entry
	inner  browser.SessionID
}

// entry is one browser, or one that is still starting.
//
// ready is closed when the launch finishes, successfully or not. A second caller for the same profile
// waits on it instead of launching in parallel — see the package comment on why two Chromes over one
// profile directory is worse than slow.
type entry struct {
	ref profile.Ref
	// fenced is the whole placement reduced to one comparable string — the sites this browser
	// admits AND the ones it may submit a form to. Both, and it used to be only the first: a write
	// grant withdrawn while a browser was up changed no origin, so the fingerprint matched, and the
	// pool handed back a browser still fenced by the permission that had just been taken away. A
	// revocation that does not reach the process enforcing it is a revocation in name.
	fenced string
	ready  chan struct{}
	driver Instance
	err    error
	// human marks the browser a person is driving. It is not a property of the session but of the
	// BROWSER, because that is what spec §4.1 bounds: one process per profile, and while it is the
	// headful one there is nowhere for an agent session on that profile to be put.
	human bool

	sessions map[browser.SessionID]struct{}
}

// New builds a pool. maxSessions is spec §8's max_sessions; limits are its profile ceilings.
func New(launcher Launcher, store profile.Store, limits profile.Limits, maxSessions int) *Pool {
	if maxSessions < 1 {
		maxSessions = 1
	}
	return &Pool{
		launcher: launcher,
		store:    store,
		limits:   limits,
		max:      maxSessions,
		running:  map[profile.Ref]*entry{},
		sessions: map[browser.SessionID]placed{},
	}
}

// Name reports the driver behind the pool, because that is what a health readout means by "driver".
func (p *Pool) Name() string { return p.launcher.Name() }

// Open places a session in the profile the núcleo chose.
func (p *Pool) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	policy, err := Policy(req.Placement)
	if err != nil {
		return browser.Session{}, err
	}

	if err := p.reserve(); err != nil {
		return browser.Session{}, err
	}
	// Held for the whole open, navigation included, and given back once the session is registered
	// and counted in its own right.
	defer p.unreserve()

	holder, err := p.acquire(ctx, req.Placement, policy)
	if err != nil {
		return browser.Session{}, err
	}

	// Outside every lock. A navigation takes seconds, and holding the pool's mutex across one would
	// stall every snapshot and every act on every other session.
	session, err := holder.driver.Open(ctx, req)
	if err != nil {
		p.release(ctx, holder, "")
		return browser.Session{}, err
	}

	p.mu.Lock()
	p.counter++
	outer := browser.SessionID(fmt.Sprintf("s%d", p.counter))
	holder.sessions[outer] = struct{}{}
	p.sessions[outer] = placed{holder: holder, inner: session.ID}
	p.mu.Unlock()

	session.ID = outer
	return session, nil
}

func (p *Pool) Snapshot(ctx context.Context, id browser.SessionID, req browser.SnapshotRequest) (browser.Snapshot, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.Snapshot{}, err
	}
	snapshot, err := session.holder.driver.Snapshot(ctx, session.inner, req)
	if err != nil {
		return browser.Snapshot{}, err
	}
	snapshot.SessionID = id
	return snapshot, nil
}

func (p *Pool) Act(ctx context.Context, id browser.SessionID, action browser.Action) (browser.ActResult, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.ActResult{}, err
	}
	return session.holder.driver.Act(ctx, session.inner, action)
}

func (p *Pool) Screenshot(ctx context.Context, id browser.SessionID) ([]byte, error) {
	session, err := p.lookup(id)
	if err != nil {
		return nil, err
	}
	return session.holder.driver.Screenshot(ctx, session.inner)
}

func (p *Pool) Look(ctx context.Context, id browser.SessionID) (browser.LookResult, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.LookResult{}, err
	}
	return session.holder.driver.Look(ctx, session.inner)
}

func (p *Pool) Handoff(ctx context.Context, id browser.SessionID, reason string) (browser.HandoffTicket, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.HandoffTicket{}, err
	}
	ticket, err := session.holder.driver.Handoff(ctx, session.inner, reason)
	if err != nil {
		return browser.HandoffTicket{}, err
	}
	ticket.SessionID = id
	return ticket, nil
}

// Close ends a session, and takes the browser and the profile with it when it was the last one.
func (p *Pool) Close(ctx context.Context, id browser.SessionID) error {
	session, err := p.lookup(id)
	if err != nil {
		return err
	}
	closeErr := session.holder.driver.Close(ctx, session.inner)
	p.release(ctx, session.holder, id)
	return closeErr
}

// Shutdown stops every browser and discards every ephemeral profile.
//
// It is not optional housekeeping. Spec §9.3 measured that the process the launcher spawned is not
// the browser: without this, the sidecar exits, Chrome keeps running with our argv, the profile stays
// locked, and the next launch silently inherits it.
func (p *Pool) Shutdown(ctx context.Context) {
	p.mu.Lock()
	holders := make([]*entry, 0, len(p.running))
	for _, holder := range p.running {
		holders = append(holders, holder)
	}
	p.running = map[profile.Ref]*entry{}
	p.sessions = map[browser.SessionID]placed{}
	p.mu.Unlock()

	for _, holder := range holders {
		<-holder.ready
		if holder.driver != nil {
			holder.driver.Shutdown(ctx)
		}
		p.discard(holder.ref)
	}
}

// reserve takes one of spec §9.6's session slots, or refuses.
func (p *Pool) reserve() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	live := len(p.sessions) + p.pending
	if live >= p.max {
		return fmt.Errorf("%w: %d of %d", ErrTooManySessions, live, p.max)
	}
	p.pending++
	return nil
}

func (p *Pool) unreserve() {
	p.mu.Lock()
	p.pending--
	p.mu.Unlock()
}

// acquire returns the browser for a placement, launching it if it is not already up.
func (p *Pool) acquire(ctx context.Context, placement browser.Placement, policy fence.Policy) (*entry, error) {
	fingerprint := fingerprintOf(placement)

	for {
		p.mu.Lock()
		existing, running := p.running[placement.Profile]
		if !running {
			starting := &entry{
				ref:      placement.Profile,
				fenced:   fingerprint,
				ready:    make(chan struct{}),
				sessions: map[browser.SessionID]struct{}{},
			}
			p.running[placement.Profile] = starting
			p.mu.Unlock()
			p.start(ctx, starting, policy)
			<-starting.ready
			if starting.err != nil {
				return nil, starting.err
			}
			return starting, nil
		}
		p.mu.Unlock()

		<-existing.ready
		if existing.err != nil {
			// The launch that owns this entry failed and has already removed it. Go round again
			// rather than reporting somebody else's failure as this caller's.
			continue
		}
		if existing.human {
			// Spec §4.1 and §4.4: while a person is driving this profile, the agent does not get a
			// browser in it. Refused rather than queued or relaunched — relaunching would take the
			// window out from under someone mid-login, and queueing would hold an HTTP request open
			// for as long as a person takes, which spec §4.4 rule 2 says has no bound at all.
			return nil, fmt.Errorf("%w: %s", browser.ErrPersonIsDriving, placement.Profile)
		}
		if existing.fenced == fingerprint {
			return existing, nil
		}

		// The list changed. If nothing is open on this profile the pool can honour it by relaunching;
		// otherwise it refuses, because a policy swap under a live page is a fence that changes its
		// mind mid-document.
		p.mu.Lock()
		if len(existing.sessions) > 0 {
			p.mu.Unlock()
			return nil, fmt.Errorf("%w: %s", ErrPolicyChanged, placement.Profile)
		}
		delete(p.running, placement.Profile)
		p.mu.Unlock()
		existing.driver.Shutdown(ctx)
	}
}

// start launches into an entry that is already published, so a concurrent Open for the same profile
// finds it and waits instead of starting a second browser over the same directory.
func (p *Pool) start(ctx context.Context, holder *entry, policy fence.Policy) {
	defer close(holder.ready)

	fail := func(err error) {
		holder.err = err
		p.mu.Lock()
		// Only if it is still ours: a Shutdown may have taken the map out from under us.
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
	driver, err := p.launcher.Launch(ctx, dir, policy)
	if err != nil {
		// A launch that failed may still have left a directory behind; an ephemeral one is rubbish
		// from this moment and nothing else will ever come back for it.
		p.discard(holder.ref)
		fail(err)
		return
	}
	holder.driver = driver
}

// release drops a session and, when it was the last one, the browser and the profile with it.
//
// Shutting down an idle browser rather than keeping it warm costs a relaunch on the next session and
// buys three things: the graceful close that flushes the profile to disk (spec §4.2), the hundreds of
// megabytes §9.6 cares about, and — on Windows especially — a directory nothing is holding open when
// an ephemeral profile has to be deleted.
func (p *Pool) release(ctx context.Context, holder *entry, id browser.SessionID) {
	p.mu.Lock()
	if id != "" {
		delete(holder.sessions, id)
		delete(p.sessions, id)
	}
	// Retire it only if it is idle AND still the browser this profile points at. If it is not, a
	// Shutdown or a relaunch already took it out of the map and has already stopped it — stopping it
	// again here would be the second close of a browser somebody else is responsible for.
	retire := len(holder.sessions) == 0 && p.running[holder.ref] == holder
	if retire {
		delete(p.running, holder.ref)
	}
	p.mu.Unlock()

	if !retire {
		return
	}
	holder.driver.Shutdown(ctx)
	p.discard(holder.ref)
}

// discard removes an ephemeral profile from disk. A project profile is left alone by
// profile.Store.Discard itself, which is where that refusal belongs.
//
// # Why it chases instead of asking once
//
// It used to be one call with the error thrown away, and the promise underneath it — spec §5.1, an
// ephemeral profile dies with its run — quietly did not hold. The removal RACES the browser's own
// death. Stop() issues `taskkill /T /F` and then waits on the launcher's pid, which spec §9.3 says
// is not the browser: the renderers it just killed can still be exiting, still holding handles into
// the profile directory, when RemoveAll walks it. On Windows an open handle is enough to make a file
// undeletable, so the call fails, and `_ =` meant nothing anywhere said so.
//
// MEASURED against the gate: the assertion that the directory is gone the instant the last session
// closes fails intermittently and passes in isolation, which is the shape of a race and not of a
// slow machine. The window is short — the handles go as the processes finish — so this asks again
// rather than waiting once for a guessed interval, and the total is bounded because a profile that
// something is holding open FOREVER is a different fault and must not become a hang here.
//
// The final failure is still swallowed, and that is a decision rather than an oversight: this
// package has nowhere to report to, and Store.SweepEphemeral takes the leftovers at the next start.
// It is a backstop and not a fix — until that start, a profile the spec says is gone is on disk.
func (p *Pool) discard(ref profile.Ref) {
	if ref.Persistent() {
		return
	}
	_ = chase(func() error { return p.store.Discard(ref) }, discardWithin, discardRetry)
}

// chase repeats an attempt until it succeeds or the window closes, and hands back the last error.
//
// A free function taking the attempt rather than a loop inside discard, because the thing worth
// testing is the chasing and the caller it exists for cannot be made to fail on demand: profile.Store
// is a concrete type over a real directory, so a test that went in through discard would be a test of
// the filesystem's mood on the day it ran.
//
// It always attempts at least once, including when the window is zero or negative. A caller that
// passed no window meant "try", not "do nothing".
func chase(attempt func() error, within, gap time.Duration) error {
	deadline := time.Now().Add(within)
	for {
		err := attempt()
		if err == nil || !time.Now().Before(deadline) {
			return err
		}
		time.Sleep(gap)
	}
}

func (p *Pool) lookup(id browser.SessionID) (placed, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	session, ok := p.sessions[id]
	if !ok {
		return placed{}, browser.ErrNoSuchSession
	}
	return session, nil
}

// fingerprintOf reduces a placement's two lists to something comparable.
//
// Normalised and sorted, so that a list the núcleo happened to send in a different order does not
// look like a different policy and tear down a working browser. Entries that normalise to nothing are
// dropped here for the same reason fence.Policy rejects them: they must not be the difference between
// two fingerprints when they are not the difference between two policies.
//
// BOTH lists, and they are kept apart by the separator rather than merged: a profile that may read
// two sites and write to neither must not fingerprint the same as one that may read one and write to
// the other, and concatenating the two lists into one bag is exactly how it would.
func fingerprintOf(placement browser.Placement) string {
	return normalisedList(placement.Origins) + " | " + normalisedList(placement.Writable)
}

func normalisedList(origins []string) string {
	normalised := make([]string, 0, len(origins))
	for _, origin := range origins {
		if entry := fence.NormaliseEntry(origin); entry != "" {
			normalised = append(normalised, entry)
		}
	}
	sort.Strings(normalised)
	return strings.Join(normalised, " ")
}
