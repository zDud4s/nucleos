// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"strings"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// fenceCallTimeout bounds every answer to a paused request.
//
// It is generous because the cost of being late is bounded and the cost of giving up is not: a
// paused request that never gets an answer wedges the renderer, and a wedged renderer looks exactly
// like a page the fence blocked. The spike made that mistake and recorded a false PASS for it, which
// is why this timeout exists at all rather than the context being inherited from nothing.
const fenceCallTimeout = 20 * time.Second

// fetchPaused is Fetch.requestPaused. The response fields are pointers and empty strings because
// their ABSENCE is the signal: a paused request with no responseStatusCode is at the request stage,
// where the method rule applies; one with a status code is at the response stage, where the download
// rule and the CSP do.
type fetchPaused struct {
	RequestID string `json:"requestId"`
	Request   struct {
		URL     string            `json:"url"`
		Method  string            `json:"method"`
		Headers map[string]string `json:"headers"`
	} `json:"request"`
	FrameID             string         `json:"frameId"`
	ResourceType        string         `json:"resourceType"`
	ResponseErrorReason string         `json:"responseErrorReason"`
	ResponseStatusCode  *int           `json:"responseStatusCode"`
	ResponseHeaders     []fence.Header `json:"responseHeaders"`
}

func (p fetchPaused) atResponseStage() bool {
	return p.ResponseStatusCode != nil || p.ResponseErrorReason != ""
}

// onFetchPaused is the fence.
//
// Every path through this function answers the request. That is not tidiness — it is the single
// invariant the whole file is arranged around, and the reason continueRequest and continueResponse
// both fall back to failRequest instead of returning. See [answerOrFail].
func (d *Driver) onFetchPaused(event cdp.Event) {
	if event.Method != "Fetch.requestPaused" {
		return
	}
	var paused fetchPaused
	if err := json.Unmarshal(event.Params, &paused); err != nil || paused.RequestID == "" {
		// Without a requestId there is no handle to answer with, so there is genuinely nothing to
		// do. The renderer waits out its own timeout. Worth knowing this branch exists: it is the
		// only way a request goes unanswered, and it requires Chrome to have sent something
		// unparseable.
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), fenceCallTimeout)
	defer cancel()

	if paused.atResponseStage() {
		d.answerResponse(ctx, event.Session, paused)
		return
	}
	d.answerRequest(ctx, event.Session, paused)
}

func (d *Driver) answerRequest(ctx context.Context, on cdp.SessionID, paused fetchPaused) {
	request := fence.Request{
		Method:       paused.Request.Method,
		URL:          paused.Request.URL,
		ResourceType: paused.ResourceType,
		Headers:      paused.Request.Headers,
	}

	// The write rule's fifth condition, and the only part of the fence that is about an instant
	// rather than about configuration: was this submission caused by an act? Looked up before the
	// decision and CONSUMED after it, so a POST refused for something else — the wrong origin, a
	// profile with no grant — does not also burn the window the act opened.
	//
	// NeedsWriteWindow rather than a method check written out here, so this side cannot drift from
	// the side that judges: a request Decide judges by the write rule and that arrives with Armed
	// unfilled would be refused for a reason that is true of the field and false of the world.
	var owner *session
	var window *writeWindow
	if fence.NeedsWriteWindow(request) {
		owner, window = d.armedWrite(paused.FrameID, fence.WriteOriginOf(request.URL))
		if window != nil {
			request.Armed = window.origin
		}
	}

	verdict := fence.Decide(d.policy, request)
	if !verdict.Allow {
		d.recordRefusal(on, verdict)
		d.failRequest(ctx, on, paused.RequestID)
		return
	}
	if window != nil {
		// Only now, and only here. What is written down is what LEFT — a submission the fence
		// stopped is a refusal, and the record of what an agent did as the person must not contain
		// things it did not do.
		d.takeWrite(owner, window, request.Method, request.URL)
	}

	params := map[string]any{"requestId": paused.RequestID}
	if wantsResponseStage(paused.ResourceType) {
		// Ask for the second pause only where it buys something. A document needs it for the CSP,
		// and "Other" is how Chrome classifies the fetch behind a download that never becomes a
		// document. Images, scripts and stylesheets get waved through in one hop: a second pause per
		// asset would double the interception cost of every page for a check that cannot fire.
		params["interceptResponse"] = true
	}
	d.answerOrFail(ctx, on, paused.RequestID, "Fetch.continueRequest", params)
}

func (d *Driver) answerResponse(ctx context.Context, session cdp.SessionID, paused fetchPaused) {
	verdict := fence.DecideResponse(d.policy, fence.Response{
		URL:          paused.Request.URL,
		ResourceType: paused.ResourceType,
		Headers:      fence.HeadersFrom(paused.ResponseHeaders),
	})
	if !verdict.Allow {
		d.recordRefusal(session, verdict)
		d.failRequest(ctx, session, paused.RequestID)
		return
	}

	// Before the CSP branch and on a condition of its own, because the two ask different questions:
	// the header goes on a document that has not got one yet, and the status is worth keeping from
	// EVERY document response — including one already carrying the fence, which is the same page
	// coming through a second time.
	if isDocumentType(paused.ResourceType) && paused.ResponseStatusCode != nil {
		d.recordStatus(paused.FrameID, *paused.ResponseStatusCode)
	}

	params := map[string]any{"requestId": paused.RequestID}
	if isDocumentType(paused.ResourceType) && !fence.CarriesFence(paused.ResponseHeaders) && paused.ResponseStatusCode != nil {
		// The CSP goes on every document, in a frame or not. Spec §5.4's earlier version said "top
		// level", and an iframe is a document that is not top level — which was the gap.
		//
		// Both fields together or neither, MEASURED: continueResponse answers "Cannot override only
		// status or headers, both should be provided". An earlier version sent the status on every
		// response and the headers only on documents, so every non-document response-stage pause was
		// rejected and fell through to failRequest — which looked exactly like the fence working, and
		// blocked the page it was supposed to let through.
		params["responseCode"] = *paused.ResponseStatusCode
		params["responseHeaders"] = fence.InjectCSP(paused.ResponseHeaders)
	}
	d.answerOrFail(ctx, session, paused.RequestID, "Fetch.continueResponse", params)
}

// answerOrFail sends an answer and, if the browser refuses it, falls back to failing the request.
//
// The fallback is the point. continueResponse can be rejected for reasons that have nothing to do
// with policy — a request already torn down, a Chrome that renamed a parameter — and the tempting
// thing to write is a return. A return leaves the request paused, which does not mean "blocked": it
// means the renderer is stuck, and a test asserting "the page did not load" passes for the wrong
// reason. That is exactly how the spike produced a false PASS (§11).
func (d *Driver) answerOrFail(ctx context.Context, session cdp.SessionID, requestID, method string, params map[string]any) {
	if _, err := d.conn.Call(ctx, session, method, params); err == nil {
		return
	}
	d.failRequest(ctx, session, requestID)
}

func (d *Driver) failRequest(ctx context.Context, session cdp.SessionID, requestID string) {
	_, _ = d.conn.Call(ctx, session, "Fetch.failRequest", map[string]any{
		"requestId": requestID,
		// BlockedByClient is what an extension-blocked request looks like, so pages that handle
		// being blocked at all handle this. The alternative, Aborted, is indistinguishable from a
		// user navigating away.
		"errorReason": "BlockedByClient",
	})
}

func isDocumentType(resourceType string) bool {
	return strings.EqualFold(resourceType, "Document")
}

func wantsResponseStage(resourceType string) bool {
	return isDocumentType(resourceType) || strings.EqualFold(resourceType, "Other")
}

// recordedRefusal is one thing the fence stopped, kept so the act that caused it can report it.
//
// Session is "" when the CDP session could not be traced back to one of ours — an out-of-process
// iframe, or an event delivered on the browser session. Act treats an unattributed refusal as its
// own, which is right when one session is acting and imprecise when several are at once. Named here
// rather than hidden: the alternative is dropping the refusal, and a fence that stops something
// without telling the agent is a fence the agent will walk into again on the next turn.
type recordedRefusal struct {
	Session     browser.SessionID
	Consequence browser.Consequence
	Detail      string
}

// refusalCap bounds the record for the same reason the proxy's does: a page in a loop generates
// refusals faster than anything reads them.
const refusalCap = 256

// recordStatus keeps the HTTP status of the PAGE.
//
// # Why the frame id and not the session
//
// Every other recorder here resolves cdpToSession[event.Session] and this one cannot: Fetch.enable
// is on the BROWSER session (Connect, and spec §5.8 for why it has to be), so every paused request
// in the whole browser arrives under that one id. It names no page. A first version looked the
// session up anyway, found nothing, and quietly recorded no status at all — which the gate caught
// only because it asserted a number rather than the absence of one.
//
// The frame id does name a page, and it is the only thing in the event that does. It is minted per
// frame across the browser, and a session's main frame is fixed when the session is created and
// never reassigned, so matching on it keeps meaning the same thing for the life of the session and
// across every navigation in it.
//
// # Why no match is the right answer for a frame
//
// The status is the PAGE's. An advertisement, a widget or a tracker that 404s inside an iframe says
// nothing about whether the article loaded, and reporting it as the page's status would be worse
// than reporting nothing: the agent would abandon a page that is perfectly fine, with the reading
// agreeing. A subframe's id matches no session's main frame, so it falls out of this loop unrecorded
// — the exclusion is the loop's ordinary behaviour rather than a rule that could be forgotten.
func (d *Driver) recordStatus(frame string, status int) {
	if frame == "" {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, entry := range d.sessions {
		if entry.frameID != frame {
			continue
		}
		// Overwritten rather than accumulated, and deliberately NOT cleared when the document
		// changes: a redirect is two document responses on one frame and the last one is what
		// arrived, and the response reaches here BEFORE the navigation finishes — so clearing on
		// navigation would throw away the status of the page being navigated to.
		entry.status = status
		return
	}
}

func (d *Driver) recordRefusal(session cdp.SessionID, verdict fence.Verdict) {
	d.mu.Lock()
	owner := d.cdpToSession[session]
	d.mu.Unlock()
	d.recordSessionRefusal(owner, verdict.Consequence, verdict.Detail)
}

// recordSessionRefusal is the one place a refusal is written down, so refusalTotal and the ring can
// never disagree about how many there have been. The total is separate from len(refusals) precisely
// because the ring forgets: an act that started before an old refusal aged out must not be told that
// the count went backwards.
func (d *Driver) recordSessionRefusal(id browser.SessionID, consequence browser.Consequence, detail string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.refusals = append(d.refusals, recordedRefusal{Session: id, Consequence: consequence, Detail: detail})
	d.refusalTotal++
	if len(d.refusals) > refusalCap {
		d.refusals = d.refusals[len(d.refusals)-refusalCap:]
	}
}

func (d *Driver) refusalCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.refusalTotal
}

// refusalSettle is how long an act waits to see whether the fence stopped what it started.
//
// A click that triggers a request is not synchronous with the request, so there is no event to wait
// on — only a window. MEASURED against real Chrome: a form submission from a click takes well over
// the 300ms this was first set to, and the first gate run duly reported "done" for a POST the fence
// had stopped — the exact failure spec §6.2's second half exists to prevent.
//
// The window is not the whole answer, because no fixed window can be. A refusal that arrives after it
// is carried over and reported on the session's NEXT act (see session.reportedUpTo), so the agent
// learns late rather than never.
const refusalSettle = 1500 * time.Millisecond

// refusalFor waits briefly for a refusal newer than `since` belonging to this session.
func (d *Driver) refusalFor(ctx context.Context, id browser.SessionID, since int) *recordedRefusal {
	found, _ := d.refusalForAt(ctx, id, since)
	return found
}

// refusalForAt is refusalFor with the absolute index of what it found, so a caller can record how
// far it has consumed and not report the same refusal twice.
func (d *Driver) refusalForAt(ctx context.Context, id browser.SessionID, since int) (*recordedRefusal, int) {
	deadline := time.NewTimer(refusalSettle)
	defer deadline.Stop()
	tick := time.NewTicker(15 * time.Millisecond)
	defer tick.Stop()

	for {
		if found, at := d.newRefusal(id, since); found != nil {
			return found, at
		}
		select {
		case <-deadline.C:
			return d.newRefusal(id, since)
		case <-ctx.Done():
			return nil, since
		case <-tick.C:
		}
	}
}

func (d *Driver) newRefusal(id browser.SessionID, since int) (*recordedRefusal, int) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.refusalTotal <= since {
		return nil, since
	}
	// The slice is a bounded ring, so translate the absolute count into an index into what is left.
	skip := since - (d.refusalTotal - len(d.refusals))
	if skip < 0 {
		skip = 0
	}
	first := d.refusalTotal - len(d.refusals)
	for offset, refusal := range d.refusals[skip:] {
		if refusal.Session == id || refusal.Session == "" {
			found := refusal
			return &found, first + skip + offset + 1
		}
	}
	return nil, d.refusalTotal
}
