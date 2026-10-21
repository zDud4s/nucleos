// §spec pilar-de-browser

package pool

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/chrome"
	"nucleosbrowser/fence"
)

// Embedded attach mode: the browser is the person's WebView2, not ours. The pool fences ONE tab of it
// per project — a proxy the daemon points the WebView2 at, plus a CDP connection that adopts the tab
// the shell opened as about:blank#<nonce> — and routes the adopted session like any other. The browser
// itself is never launched, shut down, or given a profile directory by this file.

const (
	// embeddedHTTPTimeout bounds one DevTools HTTP request; the endpoint is on this machine.
	embeddedHTTPTimeout = 5 * time.Second
	// embeddedDialTimeout bounds the WebSocket handshake.
	embeddedDialTimeout = 5 * time.Second
	// embeddedBodyLimit bounds a /json/list or /json/version answer.
	embeddedBodyLimit = 1 << 20
)

// findTargetWithin and findTargetEvery bound how long findTarget waits for the tab to be listed.
// Variables so a test can shorten them.
var (
	findTargetWithin = 2 * time.Second
	findTargetEvery  = 75 * time.Millisecond
)

// defaultEmbeddedHTTP never follows a redirect and never uses the environment's proxy: the endpoint is
// loopback and must answer there.
var defaultEmbeddedHTTP = &http.Client{
	Timeout:   embeddedHTTPTimeout,
	Transport: &http.Transport{Proxy: nil},
	CheckRedirect: func(*http.Request, []*http.Request) error {
		return http.ErrUseLastResponse
	},
}

// embeddedProject is the state of one project: its proxy, and the CDP connection into the WebView2.
type embeddedProject struct {
	name        string
	placement   browser.Placement
	fingerprint string
	policy      fence.Policy
	proxy       *fence.Proxy

	// holder and attaching are guarded by Pool.mu. holder is the entry of the CURRENT connection and
	// is nil before the first attach; attaching counts attaches in flight.
	holder    *entry
	attaching int

	// attach serialises attaches and is held across network calls; mu guards the connection state and
	// is never held across one, so a forget can always reach the connection.
	attach  sync.Mutex
	mu      sync.Mutex
	cdpHTTP string
	conn    *cdp.Conn
	driver  *chrome.Driver
	dead    bool
}

// shutdown closes the proxy and the connection. It leaves the person's browser and its tab alone.
func (e *embeddedProject) shutdown() {
	_ = e.proxy.Close()
	e.mu.Lock()
	e.dead = true
	conn := e.conn
	e.conn = nil
	e.driver = nil
	e.mu.Unlock()
	if conn != nil {
		_ = conn.Close()
	}
}

// embeddedInstance lets the chrome driver sit in an entry. Shutdown does nothing, by design: the
// WebView2 is the person's, and ending a session never ends it.
type embeddedInstance struct{ *chrome.Driver }

func (embeddedInstance) Shutdown(context.Context) {}

// PrepareEmbedded starts the project's fence proxy, or returns the running one.
//
// Idempotent: the same project with the same placement answers the same address. A different site
// list is refused with ErrEmbeddedPolicyChanged while a session (or an attach) of the project is
// live — a policy swap under a live page is a fence that changes its mind mid-document — and
// otherwise recreates the proxy and drops the idle connection, as acquire relaunches an idle browser.
func (p *Pool) PrepareEmbedded(_ context.Context, req browser.PrepareRequest) (browser.PrepareReply, error) {
	if strings.TrimSpace(req.Project) == "" {
		return browser.PrepareReply{}, fmt.Errorf("%w: project is required", browser.ErrBadAttach)
	}
	policy, err := Policy(req.Placement)
	if err != nil {
		return browser.PrepareReply{}, err
	}
	fingerprint := fingerprintOf(req.Placement)

	p.mu.Lock()
	existing := p.embeddedProjects[req.Project]
	if existing != nil && existing.fingerprint == fingerprint && existing.placement.Profile == req.Placement.Profile {
		addr := existing.proxy.Addr()
		p.mu.Unlock()
		return browser.PrepareReply{ProxyAddr: addr}, nil
	}
	if existing != nil && (existing.attaching > 0 || (existing.holder != nil && len(existing.holder.sessions) > 0)) {
		p.mu.Unlock()
		return browser.PrepareReply{}, fmt.Errorf("%w: %w: %s", browser.ErrEmbeddedPolicyChanged, ErrPolicyChanged, req.Project)
	}
	proxy, err := fence.NewEmbeddedProxy(policy)
	if err != nil {
		p.mu.Unlock()
		return browser.PrepareReply{}, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}
	if p.embeddedProjects == nil {
		p.embeddedProjects = map[string]*embeddedProject{}
	}
	p.embeddedProjects[req.Project] = &embeddedProject{
		name:        req.Project,
		placement:   req.Placement,
		fingerprint: fingerprint,
		policy:      policy,
		proxy:       proxy,
	}
	p.mu.Unlock()

	if existing != nil {
		// Idle by the check above: nothing registered on its connection.
		existing.shutdown()
	}
	return browser.PrepareReply{ProxyAddr: proxy.Addr()}, nil
}

// AttachEmbedded adopts the tab the shell opened for this attach and returns a session routable by
// every Pool verb.
func (p *Pool) AttachEmbedded(ctx context.Context, req browser.AttachRequest) (browser.Session, error) {
	base, err := validateCDPHTTP(req.CDPHTTP)
	if err != nil {
		return browser.Session{}, err
	}
	if strings.TrimSpace(req.Nonce) == "" {
		return browser.Session{}, fmt.Errorf("%w: nonce is required", browser.ErrBadAttach)
	}
	if strings.TrimSpace(req.URL) == "" {
		return browser.Session{}, fmt.Errorf("%w: url is required", browser.ErrBadAttach)
	}

	p.mu.Lock()
	project := p.embeddedProjects[req.Project]
	if project == nil {
		p.mu.Unlock()
		return browser.Session{}, fmt.Errorf("%w: %s", browser.ErrNotPrepared, req.Project)
	}
	if project.fingerprint != fingerprintOf(req.Placement) || project.placement.Profile != req.Placement.Profile {
		p.mu.Unlock()
		return browser.Session{}, fmt.Errorf("%w: %s", browser.ErrPlacementMismatch, req.Project)
	}
	project.attaching++
	p.mu.Unlock()
	defer func() {
		p.mu.Lock()
		project.attaching--
		p.mu.Unlock()
	}()

	if err := p.reserve(); err != nil {
		return browser.Session{}, err
	}
	defer p.unreserve()

	project.attach.Lock()
	defer project.attach.Unlock()

	targetID, err := p.findTarget(ctx, base, req.Nonce)
	if err != nil {
		return browser.Session{}, err
	}
	driver, holder, err := p.embeddedConnection(ctx, project, base)
	if err != nil {
		return browser.Session{}, err
	}

	// Outside every lock, like Open's navigation.
	session, err := driver.Adopt(ctx, targetID, req.URL)
	if err != nil {
		return browser.Session{}, err
	}

	p.mu.Lock()
	if p.embeddedProjects[req.Project] != project || project.holder != holder {
		// Forgotten (or the connection replaced) while the tab was being adopted: the forget wins, and
		// the connection that held this session is already closed.
		p.mu.Unlock()
		return browser.Session{}, fmt.Errorf("%w: %s", ErrForgotten, req.Project)
	}
	p.counter++
	outer := browser.SessionID(fmt.Sprintf("s%d", p.counter))
	holder.sessions[outer] = struct{}{}
	p.sessions[outer] = placed{holder: holder, inner: session.ID}
	p.mu.Unlock()

	session.ID = outer
	return session, nil
}

// ForgetEmbedded drops everything held for a project: its sessions' bookkeeping (ending their
// viewers), the proxy and the CDP connection. The person's tab is left as it is.
func (p *Pool) ForgetEmbedded(_ context.Context, name string) error {
	p.mu.Lock()
	project := p.embeddedProjects[name]
	delete(p.embeddedProjects, name)
	var holder *entry
	if project != nil {
		holder = project.holder
	}
	p.mu.Unlock()
	if project == nil {
		return nil
	}
	p.dropEmbeddedSessions(holder)
	project.shutdown()
	return nil
}

// embeddedDriver resolves an outer session id to the chrome driver of its embedded entry and the
// driver's own session id. A session that is not an embedded one is ErrNoSuchSession.
func (p *Pool) embeddedDriver(id browser.SessionID) (*chrome.Driver, browser.SessionID, error) {
	session, err := p.lookup(id)
	if err != nil {
		return nil, "", err
	}
	if !session.holder.embedded {
		return nil, "", browser.ErrNoSuchSession
	}
	instance, ok := session.holder.driver.(embeddedInstance)
	if !ok {
		return nil, "", browser.ErrNoSuchSession
	}
	return instance.Driver, session.inner, nil
}

// DetachEmbedded detaches the CDP target session of an adopted tab. The session stays registered and
// the browser-session fence stays armed.
func (p *Pool) DetachEmbedded(ctx context.Context, id browser.SessionID) error {
	driver, inner, err := p.embeddedDriver(id)
	if err != nil {
		return err
	}
	return driver.DetachEmbedded(ctx, inner)
}

// ReattachEmbedded attaches again to the tab of a detached session.
func (p *Pool) ReattachEmbedded(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	driver, inner, err := p.embeddedDriver(id)
	if err != nil {
		return browser.Returned{}, err
	}
	return driver.ReattachEmbedded(ctx, inner)
}

// dropEmbeddedSessions forgets every session of an embedded entry and ends its viewers.
func (p *Pool) dropEmbeddedSessions(holder *entry) {
	if holder == nil {
		return
	}
	p.mu.Lock()
	gone := make([]browser.SessionID, 0, len(holder.sessions))
	for id := range holder.sessions {
		gone = append(gone, id)
		delete(p.sessions, id)
	}
	holder.sessions = map[browser.SessionID]struct{}{}
	p.mu.Unlock()
	p.endWatchers(browser.EndGone, gone...)
	p.endPanelListeners(browser.PanelSessionClosed, gone...)
}

// embeddedConnection returns the project's driver, dialling and arming the fence when there is no
// live connection to this DevTools address. Called with project.attach held.
func (p *Pool) embeddedConnection(ctx context.Context, project *embeddedProject, base string) (*chrome.Driver, *entry, error) {
	project.mu.Lock()
	if project.dead {
		project.mu.Unlock()
		return nil, nil, fmt.Errorf("%w: %s", ErrForgotten, project.name)
	}
	if project.conn != nil && project.cdpHTTP == base && !connDone(project.conn) {
		driver := project.driver
		project.mu.Unlock()
		p.mu.Lock()
		holder := project.holder
		p.mu.Unlock()
		return driver, holder, nil
	}
	stale := project.conn
	project.conn, project.driver = nil, nil
	project.mu.Unlock()

	if stale != nil {
		// A new address or a dead connection: whatever was adopted over the old one is gone with it.
		_ = stale.Close()
		p.mu.Lock()
		old := project.holder
		project.holder = nil
		p.mu.Unlock()
		p.dropEmbeddedSessions(old)
	}

	var version struct {
		WebSocketDebuggerURL string `json:"webSocketDebuggerUrl"`
	}
	if err := p.getJSON(ctx, base+"/json/version", &version); err != nil {
		return nil, nil, err
	}
	if err := validateDebuggerURL(version.WebSocketDebuggerURL); err != nil {
		return nil, nil, err
	}
	conn, err := cdp.Dial(version.WebSocketDebuggerURL, embeddedDialTimeout)
	if err != nil {
		return nil, nil, fmt.Errorf("dialling the embedded browser: %w", err)
	}
	driver, err := chrome.ConnectEmbedded(ctx, conn, project.policy)
	if err != nil {
		_ = conn.Close()
		return nil, nil, err
	}
	holder := &entry{
		ref:      project.placement.Profile,
		fenced:   project.fingerprint,
		ready:    make(chan struct{}),
		driver:   embeddedInstance{driver},
		embedded: true,
		sessions: map[browser.SessionID]struct{}{},
	}
	close(holder.ready)
	driver.OnEmbeddedGone(func(inner browser.SessionID) { p.embeddedTabGone(holder, inner) })

	project.mu.Lock()
	if project.dead {
		project.mu.Unlock()
		_ = conn.Close()
		return nil, nil, fmt.Errorf("%w: %s", ErrForgotten, project.name)
	}
	project.conn, project.driver, project.cdpHTTP = conn, driver, base
	project.mu.Unlock()
	p.mu.Lock()
	project.holder = holder
	p.mu.Unlock()
	return driver, holder, nil
}

// embeddedTabGone is called when the person closed the tab of an adopted session. It closes the session
// through the normal path, so its pool slot, viewers and driver bookkeeping are released; without it an
// embedded session lives until the project is forgotten and the pool fills up (max_sessions).
func (p *Pool) embeddedTabGone(holder *entry, inner browser.SessionID) {
	p.mu.Lock()
	var outer browser.SessionID
	for id := range holder.sessions {
		if p.sessions[id].inner == inner {
			outer = id
			break
		}
	}
	p.mu.Unlock()
	if outer == "" {
		return
	}
	// Off the connection's event goroutine: Close makes CDP calls whose replies it delivers.
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), embeddedHTTPTimeout)
		defer cancel()
		_ = p.Close(ctx, outer)
	}()
}

func connDone(conn *cdp.Conn) bool {
	select {
	case <-conn.Done():
		return true
	default:
		return false
	}
}

// findTarget returns the id of the page whose url is about:blank#<nonce>.
func (p *Pool) findTarget(ctx context.Context, base, nonce string) (string, error) {
	want := "about:blank#" + nonce
	deadline := time.Now().Add(findTargetWithin)
	for {
		var targets []struct {
			ID   string `json:"id"`
			Type string `json:"type"`
			URL  string `json:"url"`
		}
		if err := p.getJSON(ctx, base+"/json/list", &targets); err != nil {
			return "", err
		}
		for _, target := range targets {
			if target.Type == "page" && target.URL == want && target.ID != "" {
				return target.ID, nil
			}
		}
		// The shell's ack can beat WebView2 listing the new tab: ask again for a short while.
		if !time.Now().Add(findTargetEvery).Before(deadline) {
			return "", browser.ErrNoSuchTarget
		}
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(findTargetEvery):
		}
	}
}

// getJSON GETs a DevTools HTTP endpoint with a bounded body.
func (p *Pool) getJSON(ctx context.Context, endpoint string, into any) error {
	client := defaultEmbeddedHTTP
	p.mu.Lock()
	if p.embeddedHTTP != nil {
		client = p.embeddedHTTP
	}
	p.mu.Unlock()

	ctx, cancel := context.WithTimeout(ctx, embeddedHTTPTimeout)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return err
	}
	response, err := client.Do(request)
	if err != nil {
		return fmt.Errorf("reading the embedded browser's DevTools endpoint: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("the embedded browser's DevTools endpoint answered %d", response.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, embeddedBodyLimit+1))
	if err != nil {
		return err
	}
	if len(body) > embeddedBodyLimit {
		return errors.New("the embedded browser's DevTools answer is too large")
	}
	return json.Unmarshal(body, into)
}

func isLoopbackName(host string) bool {
	return host == "127.0.0.1" || host == "localhost"
}

// validateCDPHTTP accepts only http://127.0.0.1:<port> or http://localhost:<port>, with no userinfo,
// path, query or fragment, and returns it without a trailing slash.
func validateCDPHTTP(raw string) (string, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "http" || u.User != nil || u.Opaque != "" ||
		(u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" ||
		!isLoopbackName(u.Hostname()) {
		return "", browser.ErrBadCDPAddress
	}
	port, err := strconv.Atoi(u.Port())
	if err != nil || port < 1 || port > 65535 {
		return "", browser.ErrBadCDPAddress
	}
	return "http://" + net.JoinHostPort(u.Hostname(), u.Port()), nil
}

// validateDebuggerURL requires the browser-reported WebSocket url to be loopback too.
func validateDebuggerURL(raw string) error {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme != "ws" || u.User != nil || (!isLoopbackName(u.Hostname()) && u.Hostname() != "::1") {
		return browser.ErrBadCDPAddress
	}
	return nil
}
