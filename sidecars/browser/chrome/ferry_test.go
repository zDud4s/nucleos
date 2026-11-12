// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
	"nucleosbrowser/fence"
)

// inWorld tells the driver where a page's world is, which is where the ferry's same-origin rule
// reads the origin from. Chromium reports this; the page never does.
func inWorld(fake *cdptest.Browser, origin string, contextID int64) {
	fake.Emit("S1", "Runtime.executionContextCreated", map[string]any{
		"context": map[string]any{
			"id":      contextID,
			"origin":  origin,
			"auxData": map[string]any{"frameId": "F1"},
		},
	})
}

// asks is the page calling the binding.
func asks(fake *cdptest.Browser, contextID int64, id int64, method, url string) {
	payload, _ := json.Marshal(map[string]any{"id": id, "url": url, "method": method})
	fake.Emit("S1", "Runtime.bindingCalled", map[string]any{
		"name":               ferryBinding,
		"payload":            string(payload),
		"executionContextId": contextID,
	})
}

// answered waits for the reply the ferry evaluates back into the page, and returns it decoded.
//
// Waited for rather than read, because the ferry answers on a goroutine — it has to, since handlers
// run in order on one dispatch loop and carrying a request inline would hold every refusal,
// navigation and lifecycle event behind it for as long as the network takes.
func answered(t *testing.T, fake *cdptest.Browser, within time.Duration) map[string]any {
	t.Helper()
	deadline := time.Now().Add(within)
	for time.Now().Before(deadline) {
		for _, call := range fake.Calls() {
			if call.Method != "Runtime.evaluate" {
				continue
			}
			var params struct {
				Expression string `json:"expression"`
			}
			if err := json.Unmarshal(call.Params, &params); err != nil {
				continue
			}
			open := strings.Index(params.Expression, "{")
			shut := strings.LastIndex(params.Expression, "}")
			if open < 0 || shut <= open {
				continue
			}
			var answer map[string]any
			if err := json.Unmarshal([]byte(params.Expression[open:shut+1]), &answer); err == nil {
				return answer
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("the ferry never answered; calls were %v", fake.Methods())
	return nil
}

func ferrying(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, driver := connected(t)
	session := opened(t, driver)
	inWorld(fake, "https://example.org", 7)
	return fake, driver, session.ID
}

// TestTheFerryCarriesOnlyGetAndHead.
//
// The method rule, and it is the fence's own: a write is the consequence spec §6.2 exists to refuse,
// and a service that carried one would be a way around the rule rather than a way to read a page.
func TestTheFerryCarriesOnlyGetAndHead(t *testing.T) {
	fake, driver, id := ferrying(t)

	asks(fake, 7, 1, "POST", "https://example.org/orders")

	answer := answered(t, fake, 5*time.Second)
	if answer["ok"] == true {
		t.Fatal("the ferry carried a POST")
	}
	if detail, _ := answer["detail"].(string); !strings.Contains(detail, "POST") {
		t.Errorf("the refusal does not say what was wrong: %q", detail)
	}
	if blocked := snapshotOf(t, driver, id).Blocked; blocked == nil {
		t.Error("the page was refused and the reading did not say so")
	}
}

// TestTheFerryWillNotFetchFromAHostTheProfileDoesNotAdmit.
//
// The floor under the CORS rule, and the reason widening past same-origin is not a hole. CORS is the
// SERVER's answer to "may this page read me", so on its own it would let a page pull from anywhere
// willing to say yes. The profile's own list decides which hosts are in play at all, it is the same
// list that decides whether a document from there may load, and it answers first — before the
// request is made, which for a host nobody admitted is the whole of what matters.
func TestTheFerryWillNotFetchFromAHostTheProfileDoesNotAdmit(t *testing.T) {
	fake, driver, id := ferrying(t)

	asks(fake, 7, 1, "GET", "https://elsewhere.example.net/secrets")

	answer := answered(t, fake, 5*time.Second)
	if answer["ok"] == true {
		t.Fatal("the ferry fetched from another host")
	}
	if detail, _ := answer["detail"].(string); !strings.Contains(detail, "elsewhere.example.net") {
		t.Errorf("the refusal does not name where it would have gone: %q", detail)
	}
	if blocked := snapshotOf(t, driver, id).Blocked; blocked == nil {
		t.Error("the page was refused and the reading did not say so")
	}
}

// TestAnOriginTheDriverWasNeverToldAboutIsNotCarriedFor.
//
// The origin comes from the execution context Chromium reported. A binding call from a world the
// driver has no record of is refused rather than resolved against a guess — a restriction the
// restricted thing gets to describe is not one.
func TestAnOriginTheDriverWasNeverToldAboutIsNotCarriedFor(t *testing.T) {
	fake, _, _ := ferrying(t)

	asks(fake, 99, 1, "GET", "https://example.org/data")

	if answer := answered(t, fake, 5*time.Second); answer["ok"] == true {
		t.Fatal("the ferry carried a request from a world it knows nothing about")
	}
}

// TestAPageThatKeepsAskingIsCutOff.
//
// A page that polls would otherwise have us carrying its traffic for as long as the session lives.
func TestAPageThatKeepsAskingIsCutOff(t *testing.T) {
	fake, _, _ := ferrying(t)

	for i := 0; i <= ferryBudget+1; i++ {
		asks(fake, 7, int64(i+1), "POST", "https://example.org/orders")
	}

	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if answeredWith(fake, fmt.Sprintf("%d requests", ferryBudget)) {
			return
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("a page asked past the budget and was never cut off; the answers were %q", answers(fake))
}

// answeredWith asks whether the budget refusal is among the answers, and NOT whether it is the last
// one. It used to ask for the last, and that was a race the test lost about one run in twenty: the
// requests are carried concurrently, so a refusal for one of the early asks can be written after the
// refusal for a later one. The claim being made is "a page that keeps asking gets cut off", and the
// ordering of the answers was never part of it — but a failure looked exactly like the fence
// refusing for the wrong reason, which cost an investigation.
func answeredWith(fake *cdptest.Browser, said string) bool {
	for _, answer := range answers(fake) {
		if strings.Contains(answer, said) {
			return true
		}
	}
	return false
}

func answers(fake *cdptest.Browser) []string {
	var said []string
	for _, call := range fake.Calls() {
		if call.Method != "Runtime.evaluate" {
			continue
		}
		var params struct {
			Expression string `json:"expression"`
		}
		if err := json.Unmarshal(call.Params, &params); err == nil {
			said = append(said, params.Expression)
		}
	}
	return said
}

// setCookieCalls is every cookie the driver put back into the profile, with the url it was put
// under.
func setCookieCalls(t *testing.T, fake *cdptest.Browser) []map[string]any {
	t.Helper()
	var out []map[string]any
	for _, call := range fake.Calls() {
		if call.Method != "Network.setCookies" {
			continue
		}
		var params struct {
			Cookies []map[string]any `json:"cookies"`
		}
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("setCookies params: %v", err)
		}
		out = append(out, params.Cookies...)
	}
	return out
}

// TestACookieSetOnARedirectIsKeptUnderTheUrlThatSetIt.
//
// The ferry used to keep only the last response's cookies, and under the url the PAGE asked for.
// A Set-Cookie on a hop belongs to that hop, and one scoped by Path to the final url was filed
// against a url it does not match — the profile and the server disagreeing about a session.
func TestACookieSetOnARedirectIsKeptUnderTheUrlThatSetIt(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/start":
			http.SetCookie(w, &http.Cookie{Name: "hop", Value: "1"})
			http.Redirect(w, r, "/final", http.StatusFound)
		case "/final":
			http.SetCookie(w, &http.Cookie{Name: "end", Value: "2"})
			_, _ = io.WriteString(w, "ok")
		}
	}))
	defer server.Close()

	fake, driver := connectedUnder(t, fence.Policy{Profile: fence.Ephemeral, Loopback: []string{server.URL}})
	_, _, _, err := driver.carry(context.Background(), "S1", server.URL, server.URL+"/start", true, false)
	if err != nil {
		t.Fatalf("carry: %v", err)
	}

	kept := map[string]string{}
	for _, cookie := range setCookieCalls(t, fake) {
		kept[fmt.Sprint(cookie["name"])] = fmt.Sprint(cookie["url"])
	}
	if kept["hop"] != server.URL+"/start" {
		t.Errorf("the redirect's cookie was kept under %q, want %q", kept["hop"], server.URL+"/start")
	}
	if kept["end"] != server.URL+"/final" {
		t.Errorf("the final cookie was kept under %q, want %q", kept["end"], server.URL+"/final")
	}
}

// loopbackResolver answers every name with this machine, which is what `127.0.0.1.nip.io` and a
// rebinding domain both do.
type loopbackResolver struct{}

func (loopbackResolver) LookupIPAddr(context.Context, string) ([]net.IPAddr, error) {
	return []net.IPAddr{{IP: net.ParseIP("127.0.0.1")}}, nil
}

// TestTheFerryWillNotReachThisMachineThroughAName.
//
// The ferry dialled by name with the default transport, so a name the policy admitted that RESOLVED
// to 127.0.0.1 reached whatever listened there — the núcleo's API on 8791 included — and it honoured
// HTTPS_PROXY besides, which is a way out of the fence nobody wrote down.
func TestTheFerryWillNotReachThisMachineThroughAName(t *testing.T) {
	var reached atomic.Bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		reached.Store(true)
		_, _ = io.WriteString(w, "the daemon")
	}))
	defer server.Close()
	_, port, _ := net.SplitHostPort(strings.TrimPrefix(server.URL, "http://"))

	previous := ferryDialer.Resolver
	ferryDialer.Resolver = loopbackResolver{}
	defer func() { ferryDialer.Resolver = previous }()

	_, driver := connectedUnder(t, fence.Policy{Profile: fence.Ephemeral})
	target := "http://127.0.0.1.nip.io:" + port + "/"
	if _, _, _, err := driver.carry(context.Background(), "S1", target, target, false, false); err == nil {
		t.Fatal("the ferry carried a request to a name that resolves to this machine")
	}
	if reached.Load() {
		t.Fatal("the loopback server was reached")
	}
	if ferryTransport.Proxy != nil {
		t.Fatal("the ferry's transport consults an environment proxy")
	}
}

// TestAnUncredentialedRequestKeepsNoCookies is fetch's own rule: credentials "omit" means the
// answer's Set-Cookie is ignored, not filed into the profile behind the page's back.
func TestAnUncredentialedRequestKeepsNoCookies(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.SetCookie(w, &http.Cookie{Name: "tracker", Value: "1"})
		_, _ = io.WriteString(w, "ok")
	}))
	defer server.Close()

	fake, driver := connectedUnder(t, fence.Policy{Profile: fence.Ephemeral, Loopback: []string{server.URL}})
	if _, _, _, err := driver.carry(context.Background(), "S1", server.URL, server.URL+"/", false, false); err != nil {
		t.Fatalf("carry: %v", err)
	}
	if kept := setCookieCalls(t, fake); len(kept) != 0 {
		t.Fatalf("an uncredentialed request put %v into the profile", kept)
	}
}

// TestACrossSiteRequestCarriesOnlySameSiteNoneCookies.
//
// SameSite is the server saying which requests its cookie may ride on, relative to the site that
// STARTED the request. The ferry asked the browser for every cookie matching the url and sent the
// lot, so a page on one site reading another sent that site's Lax and Strict cookies with it — the
// exact request SameSite exists to strip them from.
func TestACrossSiteRequestCarriesOnlySameSiteNoneCookies(t *testing.T) {
	fake, driver := connected(t)
	fake.Handle("Network.getCookies", func(cdptest.Call) (any, error) {
		return map[string]any{"cookies": []map[string]any{
			{"name": "strict", "value": "s", "sameSite": "Strict"},
			{"name": "lax", "value": "l", "sameSite": "Lax"},
			{"name": "unstated", "value": "u"},
			{"name": "none", "value": "n", "sameSite": "None"},
		}}, nil
	})
	jar := &ferryJar{d: driver, ctx: context.Background(), on: "S1", initiator: "https://app.example.com", credentialed: true}

	names := func(cookies []*http.Cookie) string {
		out := make([]string, 0, len(cookies))
		for _, cookie := range cookies {
			out = append(out, cookie.Name)
		}
		return strings.Join(out, ",")
	}
	sameSiteURL, _ := url.Parse("https://api.example.com/data")
	if got := names(jar.Cookies(sameSiteURL)); got != "strict,lax,unstated,none" {
		t.Errorf("same-site request carried %q, want every cookie", got)
	}
	crossSiteURL, _ := url.Parse("https://bank.example.net/data")
	if got := names(jar.Cookies(crossSiteURL)); got != "none" {
		t.Errorf("cross-site request carried %q, want only the SameSite=None one", got)
	}
	// A suffix the public list knows is not a site: two co.uk sites are two sites.
	jar.initiator = "https://one.co.uk"
	other, _ := url.Parse("https://two.co.uk/")
	if got := names(jar.Cookies(other)); got != "none" {
		t.Errorf("one.co.uk -> two.co.uk carried %q, want only the SameSite=None one", got)
	}
	// Schemeful: http and https of the same domain are different sites.
	jar.initiator = "http://app.example.com"
	if got := names(jar.Cookies(sameSiteURL)); got != "none" {
		t.Errorf("http -> https carried %q, want only the SameSite=None one", got)
	}

	// And the other direction: a cross-site answer may set only a SameSite=None cookie.
	jar.initiator = "https://app.example.com"
	jar.SetCookies(crossSiteURL, []*http.Cookie{
		{Name: "lax", Value: "1", SameSite: http.SameSiteLaxMode},
		{Name: "none", Value: "1", SameSite: http.SameSiteNoneMode, Secure: true},
	})
	kept := setCookieCalls(t, fake)
	if len(kept) != 1 || kept[0]["name"] != "none" {
		t.Fatalf("a cross-site answer set %v, want only the SameSite=None cookie", kept)
	}
}

// TestAFerryFromTheLastDocumentDoesNotCountDownTheNextOne.
//
// A navigation zeroes the in-flight count, and a request the old document asked for finishes after
// it. Its count-down used to land on the NEW document's count, which went negative — and a page with
// its own requests still out then read as finished.
func TestAFerryFromTheLastDocumentDoesNotCountDownTheNextOne(t *testing.T) {
	_, driver, id := ferrying(t)
	entry, err := driver.lookup(id)
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}

	driver.mu.Lock()
	oldDone := driver.carryingOne(entry)
	driver.mu.Unlock()

	driver.forgetRefs(entry) // the page navigated

	driver.mu.Lock()
	newDone := driver.carryingOne(entry)
	driver.mu.Unlock()

	oldDone()
	if _, carrying := driver.ferryState(entry); carrying != 1 {
		t.Fatalf("carrying is %d after the old document's request finished, want 1", carrying)
	}
	newDone()
	if _, carrying := driver.ferryState(entry); carrying != 0 {
		t.Fatalf("carrying is %d after every request finished, want 0", carrying)
	}
}

// TestClosingASessionForgetsEverythingThatPointedAtIt is the bookkeeping half of Close: a frame's
// session, a popup's target and the ferry's execution contexts each pointed at the session, and
// were left behind for the life of the browser.
func TestClosingASessionForgetsEverythingThatPointedAtIt(t *testing.T) {
	_, driver, id := ferrying(t)

	driver.mu.Lock()
	entry := driver.sessions[id]
	driver.cdpToSession["FRAME"] = id
	entry.frames["FRAME"] = frameRef{target: "frame-target"}
	driver.targets["popup-target"] = id
	driver.contexts[contextKey{session: "FRAME", id: 3}] = executionContext{origin: "https://example.org"}
	driver.mu.Unlock()

	if err := driver.Close(context.Background(), id); err != nil {
		t.Fatalf("close: %v", err)
	}

	driver.mu.Lock()
	defer driver.mu.Unlock()
	for on, owner := range driver.cdpToSession {
		if owner == id {
			t.Errorf("cdpToSession still maps %s to the closed session", on)
		}
	}
	for target, owner := range driver.targets {
		if owner == id {
			t.Errorf("targets still maps %s to the closed session", target)
		}
	}
	if len(driver.contexts) != 0 {
		t.Errorf("execution contexts survived the session: %v", driver.contexts)
	}
}
