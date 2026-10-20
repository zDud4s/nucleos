// §spec pilar-de-browser

package browser

import (
	"context"
	"errors"
	"fmt"
)

// PrepareRequest asks for the fence proxy of one embedded project. The daemon points the person's
// WebView2 at the returned address BEFORE it opens the tab, so no request of the tab can precede the
// fence.
type PrepareRequest struct {
	Project   string    `json:"project"`
	Placement Placement `json:"placement"`
}

// PrepareReply is where the project's fence proxy listens.
type PrepareReply struct {
	ProxyAddr string `json:"proxy_addr"`
}

// AttachRequest asks the sidecar to adopt the tab the shell opened as about:blank#<nonce> in the
// person's WebView2, whose DevTools HTTP endpoint is CDPHTTP.
type AttachRequest struct {
	Project   string    `json:"project"`
	CDPHTTP   string    `json:"cdp_http"`
	Nonce     string    `json:"nonce"`
	URL       string    `json:"url"`
	Placement Placement `json:"placement"`
}

// Embedded is the optional half of a driver that can fence a browser it did not start. A driver
// that cannot does not implement it, and the routes answer 501.
type Embedded interface {
	// PrepareEmbedded starts (or finds again) the project's fence proxy. Idempotent for one policy.
	PrepareEmbedded(ctx context.Context, req PrepareRequest) (PrepareReply, error)
	// AttachEmbedded adopts the shell's tab and returns a session routable like any other.
	AttachEmbedded(ctx context.Context, req AttachRequest) (Session, error)
	// ForgetEmbedded drops everything held for a project. An unknown project is not an error.
	ForgetEmbedded(ctx context.Context, project string) error
}

var (
	// ErrBadCDPAddress means cdp_http (or the debugger url the browser reported) is not a plain
	// loopback address. The sidecar never dials anything else on the daemon's say-so.
	ErrBadCDPAddress = errors.New("browser: the CDP address must be http://127.0.0.1:<port> or http://localhost:<port>")
	// ErrBadAttach means a prepare or attach request is missing a field it cannot do without.
	ErrBadAttach = errors.New("browser: the embedded request is incomplete")
	// ErrNoSuchTarget means no page of the embedded browser carries the attach nonce.
	ErrNoSuchTarget = errors.New("browser: no page carries that nonce")
	// ErrNotPrepared means an attach arrived for a project whose proxy was never prepared.
	ErrNotPrepared = errors.New("browser: the embedded project was not prepared")
	// ErrPlacementMismatch means an attach carries a placement other than the one prepared.
	ErrPlacementMismatch = errors.New("browser: the placement differs from the one the project was prepared with")
	// ErrEmbeddedPolicyChanged means a prepare with another site list arrived while a session of
	// that project is still open. The same refusal as pool.ErrPolicyChanged, spelled where serve can
	// see it.
	ErrEmbeddedPolicyChanged = errors.New("browser: the site list changed while an embedded session was open")
)

// PrepareEmbedded on the Fake records the request and answers a fixed address.
func (f *Fake) PrepareEmbedded(_ context.Context, req PrepareRequest) (PrepareReply, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Prepared = append(f.Prepared, req)
	if f.EmbeddedErr != nil {
		return PrepareReply{}, f.EmbeddedErr
	}
	return PrepareReply{ProxyAddr: "127.0.0.1:4455"}, nil
}

// AttachEmbedded on the Fake records the request and mints a session.
func (f *Fake) AttachEmbedded(_ context.Context, req AttachRequest) (Session, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Attached = append(f.Attached, req)
	if f.EmbeddedErr != nil {
		return Session{}, f.EmbeddedErr
	}
	f.counter++
	session := Session{
		ID:           SessionID(fmt.Sprintf("s%d", f.counter)),
		Mode:         ModeAgent,
		RequestedURL: req.URL,
		FinalURL:     req.URL,
	}
	if f.sessions == nil {
		f.sessions = map[SessionID]Session{}
	}
	f.sessions[session.ID] = session
	return session, nil
}

// ForgetEmbedded on the Fake records the project.
func (f *Fake) ForgetEmbedded(_ context.Context, project string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.EmbeddedForgotten = append(f.EmbeddedForgotten, project)
	return f.EmbeddedErr
}
