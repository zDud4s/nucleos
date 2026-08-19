package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
	"unicode/utf8"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// The ferry: how a page gets its own content without the fence opening a channel for it.
//
// # The problem the CSP cannot express
//
// `connect-src 'none'` closes fetch, XHR, EventSource and beacons, and it cannot be narrowed to an
// allowlist. CSP3's scheme matching makes `https:` match `wss:` too, so no source expression admits
// a fetch while refusing a socket — and `wss:` is invisible to the other two layers: the CDP
// interception never sees it, and to the proxy it is a CONNECT byte-identical to any other. So the
// choice looked like "no modern web" or "an open bidirectional channel to hosts the person is logged
// into", and the second one is the whole of what spec §6.2 promises not to do.
//
// # The way out is not to ask CSP for a policy it cannot express
//
// The channel stays closed. What changes is that the page can ASK US to make one request, and we
// decide. A script injected before every document replaces `fetch` with one that hands the url to a
// CDP binding; the driver checks it against rules of its own and, if it passes, has the BROWSER load
// it — same profile, same cookie jar, through the fence's own interception like everything else.
//
// # Why this is not a hole
//
// The shim runs in the page's world, and a hostile page can delete it, replace it, or call the
// binding directly. None of that is an escalation, because the shim is not the boundary: it is a
// SERVICE. The boundary is [Driver.serveFerry]'s rules, which a page cannot reach around, and the
// real channel is still shut — anything the page tries outside the ferry meets `connect-src 'none'`
// exactly as before. The origin is not taken from the page either: it comes from the execution
// context Chromium reports, which the page cannot forge.
//
// # What it deliberately does not do
//
//   - Cross-origin. Same-origin only, which is the `/api/...` case and is nearly all of it. It opens
//     no host the page could not already reach, and the response comes from a server the page
//     already is.
//   - Anything but GET and HEAD. A write is the consequence §6.2 exists to refuse, and the ferry is
//     not a way around the method rule.
//   - Sockets. There is no ferry for WebSocket and there will not be one: a socket is a channel, and
//     a channel is the thing being refused.
//   - XMLHttpRequest beyond the plain asynchronous text case. What the shim cannot do properly it
//     does not do at all — it leaves the real XHR in place, which the CSP refuses and `csp.go`
//     reports. A half-working shim would fail in ways that look like the page.

const (
	// ferryBinding is what the page calls. Prefixed and unlikely, because it lands in the page's own
	// global namespace and a collision would be a page breaking for a reason nobody could see.
	ferryBinding = "__nucleosFerry"
	// ferryReply is how the answer gets back in.
	ferryReply = "__nucleosFerryReply"
	// ferryBudget bounds how many requests one document may ask us to make. A page that polls would
	// otherwise have us carrying its traffic forever; past the bound it is refused, and the refusal
	// is counted where the agent will see it.
	ferryBudget = 200
	// ferryBodyCap bounds one response. The body crosses back through a Runtime.evaluate, and a
	// megabyte of anything is already past what a page needs to render a view.
	ferryBodyCap = 4 << 20
	// ferryTimeout bounds one carried request.
	ferryTimeout = 30 * time.Second
)

// ferryShim is injected before every document.
//
// It replaces `fetch` wholesale and `XMLHttpRequest` only where it can be honest — see the package
// comment. `Response` is built from the real constructor, so a page that inspects what it got sees
// the type it expects rather than a shape we invented.
const ferryShim = `(() => {
  if (globalThis.` + ferryReply + `) { return; }
  const pending = new Map();
  let next = 0;
  globalThis.` + ferryReply + ` = (answer) => {
    const waiting = pending.get(answer.id);
    if (!waiting) { return; }
    pending.delete(answer.id);
    waiting(answer);
  };
  const ask = (url, method) => new Promise((resolve) => {
    const id = ++next;
    pending.set(id, resolve);
    try {
      ` + ferryBinding + `(JSON.stringify({id: id, url: String(url), method: String(method || 'GET')}));
    } catch (e) {
      pending.delete(id);
      resolve({id: id, ok: false, detail: String(e)});
    }
  });

  globalThis.fetch = function (input, init) {
    let url = input, method = 'GET';
    if (input && typeof input === 'object' && 'url' in input) { url = input.url; method = input.method || 'GET'; }
    if (init && init.method) { method = init.method; }
    return ask(url, method).then((answer) => {
      if (!answer.ok) { throw new TypeError('Failed to fetch: ' + (answer.detail || 'refused')); }
      return new Response(answer.body, {status: answer.status, headers: answer.headers || {}});
    });
  };

  const Real = globalThis.XMLHttpRequest;
  function Ferried() {
    this._method = 'GET'; this._url = ''; this._async = true; this._fallback = null;
    this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = '';
    this.response = ''; this.responseURL = ''; this.responseType = '';
    this.onreadystatechange = null; this.onload = null; this.onerror = null; this.onloadend = null;
    this._listeners = {}; this._headers = '';
  }
  Ferried.prototype.open = function (method, url, isAsync) {
    this._method = String(method || 'GET').toUpperCase();
    this._url = url; this._async = isAsync !== false;
    this.readyState = 1; this._fire('readystatechange');
  };
  Ferried.prototype.setRequestHeader = function () {};
  Ferried.prototype.getAllResponseHeaders = function () { return this._headers; };
  Ferried.prototype.getResponseHeader = function () { return null; };
  Ferried.prototype.abort = function () {};
  Ferried.prototype.addEventListener = function (name, fn) {
    (this._listeners[name] = this._listeners[name] || []).push(fn);
  };
  Ferried.prototype.removeEventListener = function (name, fn) {
    const all = this._listeners[name] || [];
    const at = all.indexOf(fn);
    if (at >= 0) { all.splice(at, 1); }
  };
  Ferried.prototype._fire = function (name) {
    const direct = this['on' + name];
    if (typeof direct === 'function') { try { direct.call(this, {type: name}); } catch (e) {} }
    for (const fn of (this._listeners[name] || [])) { try { fn.call(this, {type: name}); } catch (e) {} }
  };
  Ferried.prototype.send = function () {
    // Everything the shim cannot do properly, the real one still does — and the fence refuses it,
    // loudly, which is better than this pretending.
    const plain = this.responseType === '' || this.responseType === 'text' || this.responseType === 'json';
    if (!this._async || !plain) {
      const real = new Real();
      real.open(this._method, this._url, this._async);
      real.send();
      return;
    }
    ask(this._url, this._method).then((answer) => {
      if (!answer.ok) { this.readyState = 4; this._fire('readystatechange'); this._fire('error'); this._fire('loadend'); return; }
      this.status = answer.status; this.statusText = '';
      this.responseText = answer.body || '';
      this.responseURL = answer.url || this._url;
      this._headers = answer.headerText || '';
      this.response = this.responseType === 'json' ? (() => { try { return JSON.parse(this.responseText); } catch (e) { return null; } })() : this.responseText;
      this.readyState = 4;
      this._fire('readystatechange'); this._fire('load'); this._fire('loadend');
    });
  };
  Ferried.DONE = 4; Ferried.LOADING = 3; Ferried.HEADERS_RECEIVED = 2; Ferried.OPENED = 1; Ferried.UNSENT = 0;
  Ferried.prototype.DONE = 4;
  globalThis.XMLHttpRequest = Ferried;
})();`

// executionContext is what a page's world is, as far as the ferry needs to know it.
//
// Origin comes from Chromium and not from the page, which is the whole reason it is kept here: the
// same-origin rule is worth nothing if the thing being restricted gets to say where it is.
type executionContext struct {
	origin string
	frame  string
}

// armFerry installs the binding and the shim on a target.
//
// Best effort throughout, and deliberately: a session that could not be armed loses a convenience,
// not a protection. The channel is shut by the CSP either way.
func (d *Driver) armFerry(ctx context.Context, on cdp.SessionID) {
	if _, err := d.conn.Call(ctx, on, "Runtime.enable", nil); err != nil {
		return
	}
	if _, err := d.conn.Call(ctx, on, "Runtime.addBinding", map[string]any{"name": ferryBinding}); err != nil {
		return
	}
	_, _ = d.conn.Call(ctx, on, "Page.addScriptToEvaluateOnNewDocument", map[string]any{
		"source": ferryShim,
	})
}

// onRuntimeEvent tracks where a page's worlds are, and hears it ask for something.
func (d *Driver) onRuntimeEvent(event cdp.Event) {
	switch event.Method {
	case "Runtime.executionContextCreated":
		var params struct {
			Context struct {
				ID      int64  `json:"id"`
				Origin  string `json:"origin"`
				AuxData struct {
					FrameID string `json:"frameId"`
				} `json:"auxData"`
			} `json:"context"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		d.mu.Lock()
		d.contexts[contextKey{session: event.Session, id: params.Context.ID}] = executionContext{
			origin: params.Context.Origin,
			frame:  params.Context.AuxData.FrameID,
		}
		d.mu.Unlock()

	case "Runtime.executionContextsCleared":
		d.mu.Lock()
		for key := range d.contexts {
			if key.session == event.Session {
				delete(d.contexts, key)
			}
		}
		d.mu.Unlock()

	case "Runtime.bindingCalled":
		var params struct {
			Name      string `json:"name"`
			Payload   string `json:"payload"`
			ContextID int64  `json:"executionContextId"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil || params.Name != ferryBinding {
			return
		}
		// On a goroutine, always. Handlers run in order on one dispatch loop, so carrying a request
		// inline would hold up every refusal, navigation and lifecycle event behind it for as long
		// as the network takes.
		go d.serveFerry(event.Session, params.ContextID, params.Payload)
	}
}

// ferryAsk is what the shim sends.
type ferryAsk struct {
	ID     int64  `json:"id"`
	URL    string `json:"url"`
	Method string `json:"method"`
}

// ferryAnswer is what goes back.
type ferryAnswer struct {
	ID         int64             `json:"id"`
	OK         bool              `json:"ok"`
	Status     int               `json:"status,omitempty"`
	Body       string            `json:"body,omitempty"`
	URL        string            `json:"url,omitempty"`
	Headers    map[string]string `json:"headers,omitempty"`
	HeaderText string            `json:"headerText,omitempty"`
	Detail     string            `json:"detail,omitempty"`
}

// serveFerry decides on one request, carries it if it passes, and answers either way.
func (d *Driver) serveFerry(on cdp.SessionID, contextID int64, payload string) {
	var ask ferryAsk
	if err := json.Unmarshal([]byte(payload), &ask); err != nil || ask.ID == 0 {
		return
	}

	d.mu.Lock()
	owner, known := d.cdpToSession[on]
	context, placed := d.contexts[contextKey{session: on, id: contextID}]
	entry, live := d.sessions[owner]
	if live {
		entry.ferried++
	}
	spent := 0
	if live {
		spent = entry.ferried
	}
	d.mu.Unlock()

	refuse := func(detail string) {
		if known {
			d.recordBlocked(owner, browser.ConsequencePageRequest, "connect-src 'none'", ask.URL)
		}
		d.answerFerry(on, contextID, ferryAnswer{ID: ask.ID, OK: false, Detail: detail})
	}

	if !known || !placed {
		refuse("this session cannot carry requests")
		return
	}
	if spent > ferryBudget {
		refuse(fmt.Sprintf("this page has already asked for %d requests", ferryBudget))
		return
	}
	switch strings.ToUpper(ask.Method) {
	case "GET", "HEAD":
	default:
		// The method rule, and it is the same one the fence applies: a write is the consequence
		// §6.2 exists to refuse, and the ferry is not a way around it.
		refuse(strings.ToUpper(ask.Method) + " has a consequence; the fence carries GET and HEAD")
		return
	}

	target, err := url.Parse(strings.TrimSpace(ask.URL))
	if err != nil {
		refuse("that is not a url")
		return
	}
	if !target.IsAbs() {
		base, baseErr := url.Parse(context.origin)
		if baseErr != nil {
			refuse("nothing to resolve a relative url against")
			return
		}
		target = base.ResolveReference(target)
	}
	if origin := target.Scheme + "://" + target.Host; !strings.EqualFold(origin, context.origin) {
		// Same-origin, and the origin is the one Chromium reports for the calling world, never the
		// one the page claims. This opens no host the page could not already reach.
		refuse(fmt.Sprintf("the fence carries requests to %s and not to %s", context.origin, origin))
		return
	}

	ctx, cancel := context2(ferryTimeout)
	defer cancel()

	status, body, headers, err := d.carry(ctx, on, context.origin, target.String())
	if err != nil {
		refuse(err.Error())
		return
	}

	lines := make([]string, 0, len(headers))
	for name, value := range headers {
		lines = append(lines, name+": "+value)
	}
	d.answerFerry(on, contextID, ferryAnswer{
		ID:         ask.ID,
		OK:         true,
		Status:     status,
		Body:       body,
		URL:        target.String(),
		Headers:    headers,
		HeaderText: strings.Join(lines, "\r\n"),
	})
}

// context2 is context.WithTimeout, named apart because `context` is a variable in serveFerry.
func context2(within time.Duration) (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), within)
}

// carry makes the request the page asked for, and reads the body back.
//
// # Why this process and not the browser
//
// MEASURED, 2026-08-20, against the pinned build: `Network.loadNetworkResource` is the obvious
// engine — it uses the profile's own cookie jar and passes through the interception — and it cannot
// be used. Charged to a frame it inherits that frame's CSP and comes back "CSP violation", which is
// our own `connect-src 'none'` refusing it; charged to no frame it answers "Parameter frameId must
// be provided for frame targets"; and the browser session has no Network domain at all. The first
// measurement of this looked fine because it was taken against the CONTROL browser, which has no
// injected CSP — a reading that was correct about a browser nobody runs.
//
// So the request is made here. The cost is that the profile's cookies have to be fetched and put
// back by hand, which is what [Driver.cookiesFor] and [Driver.keepCookies] do.
//
// # What keeps this from being a second way out of the machine
//
// The fence's own rules, run by the fence's own code. `fence.Decide` is the same function the CDP
// interception calls on every request the browser makes, and it is called here on the same shape of
// request. On top of it sits the ferry's same-origin rule, which is stricter than anything Decide
// says — and stricter in the way that matters, since the page's own origin is by construction one
// this profile already admitted when the document loaded.
func (d *Driver) carry(ctx context.Context, on cdp.SessionID, origin, target string) (int, string, map[string]string, error) {
	verdict := fence.Decide(d.policy, fence.Request{
		Method:       "GET",
		URL:          target,
		ResourceType: "XHR",
	})
	if !verdict.Allow {
		return 0, "", nil, fmt.Errorf("%s", verdict.Detail)
	}

	request, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return 0, "", nil, err
	}
	request.Header.Set("Accept", "*/*")
	request.Header.Set("Referer", origin+"/")
	if jar := d.cookiesFor(ctx, on, target); jar != "" {
		request.Header.Set("Cookie", jar)
	}

	client := &http.Client{
		Timeout: ferryTimeout,
		// A redirect is a new request and gets the same rule. Following one off the origin is how a
		// same-origin promise turns into a cross-origin fetch without anybody deciding to.
		CheckRedirect: func(hop *http.Request, via []*http.Request) error {
			if len(via) >= 5 {
				return fmt.Errorf("too many redirects")
			}
			if hopOrigin := hop.URL.Scheme + "://" + hop.URL.Host; !strings.EqualFold(hopOrigin, origin) {
				return fmt.Errorf("it redirected to %s, which is not where the page is", hopOrigin)
			}
			return nil
		},
	}
	response, err := client.Do(request)
	if err != nil {
		return 0, "", nil, fmt.Errorf("the request did not complete: %w", err)
	}
	defer response.Body.Close()

	body, err := io.ReadAll(io.LimitReader(response.Body, ferryBodyCap+1))
	if err != nil {
		return 0, "", nil, err
	}
	if len(body) > ferryBodyCap {
		return 0, "", nil, fmt.Errorf("the answer is larger than the fence carries (%d bytes)", ferryBodyCap)
	}
	if !utf8.Valid(body) {
		// The ferry is for the text a page renders itself from. Saying so is better than handing back
		// replacement characters the page will parse as its data.
		return 0, "", nil, fmt.Errorf("the answer is not text, and the fence carries text")
	}

	d.keepCookies(ctx, on, target, response.Cookies())

	headers := make(map[string]string, len(response.Header))
	for name := range response.Header {
		headers[name] = response.Header.Get(name)
	}
	return response.StatusCode, string(body), headers, nil
}

// cookiesFor asks the BROWSER what it would have sent, which is the only place that knows.
//
// Through CDP rather than a jar of our own, because the profile is the identity (spec §4.2) and a
// second store would be a second identity that drifts from it. httpOnly cookies come back here too,
// which is what makes an authenticated API call work at all.
func (d *Driver) cookiesFor(ctx context.Context, on cdp.SessionID, target string) string {
	result, err := d.conn.Call(ctx, on, "Network.getCookies", map[string]any{"urls": []string{target}})
	if err != nil {
		return ""
	}
	var payload struct {
		Cookies []struct {
			Name  string `json:"name"`
			Value string `json:"value"`
		} `json:"cookies"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return ""
	}
	pairs := make([]string, 0, len(payload.Cookies))
	for _, cookie := range payload.Cookies {
		pairs = append(pairs, cookie.Name+"="+cookie.Value)
	}
	return strings.Join(pairs, "; ")
}

// keepCookies puts what the server set back into the profile.
//
// Without it a session-refreshing endpoint would work once and then quietly stop, and the profile
// would disagree with what the site believes about it — a divergence with no symptom until a login
// expires early.
func (d *Driver) keepCookies(ctx context.Context, on cdp.SessionID, target string, cookies []*http.Cookie) {
	if len(cookies) == 0 {
		return
	}
	entries := make([]map[string]any, 0, len(cookies))
	for _, cookie := range cookies {
		entry := map[string]any{
			"name":     cookie.Name,
			"value":    cookie.Value,
			"url":      target,
			"httpOnly": cookie.HttpOnly,
			"secure":   cookie.Secure,
		}
		if cookie.Path != "" {
			entry["path"] = cookie.Path
		}
		if cookie.Domain != "" {
			entry["domain"] = cookie.Domain
		}
		if !cookie.Expires.IsZero() {
			entry["expires"] = float64(cookie.Expires.Unix())
		}
		entries = append(entries, entry)
	}
	_, _ = d.conn.Call(ctx, on, "Network.setCookies", map[string]any{"cookies": entries})
}

// answerFerry resolves the promise the shim is holding.
func (d *Driver) answerFerry(on cdp.SessionID, contextID int64, answer ferryAnswer) {
	encoded, err := json.Marshal(answer)
	if err != nil {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	// If the page removed the function there is nothing to resolve and nothing to do about it. It
	// removed its own way of hearing the answer.
	_, _ = d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression":    fmt.Sprintf("globalThis.%s && globalThis.%s(%s)", ferryReply, ferryReply, encoded),
		"contextId":     contextID,
		"returnByValue": true,
	})
}
