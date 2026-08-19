//go:build browsergate

package gate_test

import (
	"context"
	"net"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
)

// settle is how long a test waits for something to reach the origin server.
//
// Every "it did not happen" assertion in this file is paired with a "it did happen" control that
// uses the same window, so a window that is too short fails BOTH — which is the only way a timing
// mistake here can be noticed rather than read as a fence that works.
const settle = 4 * time.Second

// ---------------------------------------------------------------------------
// Test 1 — a document from a host the profile does not admit does not execute.
// ---------------------------------------------------------------------------

func TestAnUnadmittedDocumentDoesNotRun(t *testing.T) {
	admitted := newSite(t)
	stranger := newSite(t)

	driver, _ := fenced(t, admitting(admitted))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: stranger.origin() + "/page"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal == nil {
		t.Fatalf("the navigation was not refused: %+v", session)
	}
	if stranger.reached("GET /page", settle) {
		t.Fatal("the request reached the stranger anyway")
	}

	// The control, in the same test: the admitted site loads. Without it "nothing arrived" would be
	// satisfied by a browser that could not reach anything at all.
	if _, err := driver.Open(ctx, browser.OpenRequest{URL: admitted.origin() + "/page"}); err != nil {
		t.Fatalf("the admitted site failed to open: %v", err)
	}
	if !admitted.reached("GET /page", settle) {
		t.Fatal("the admitted site was never reached either; the browser is broken, not fencing")
	}
}

func TestAnUnadmittedDocumentDoesNotRunInAFrameEither(t *testing.T) {
	admitted := newSite(t)
	stranger := newSite(t)

	driver, _ := fenced(t, admitting(admitted))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: admitted.origin() + "/page"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	// The frame is added from the page, which is how a real one arrives. Spec §5.4's earlier version
	// said "top-level navigations", and an iframe is a document that is not top level — the gap this
	// test exists for.
	drain(admitted)
	if _, err := driver.Open(ctx, browser.OpenRequest{
		URL: admitted.origin() + "/framing?src=" + stranger.origin() + "/framed",
	}); err != nil {
		t.Fatalf("open framing page: %v", err)
	}
	if stranger.reached("GET /framed", settle) {
		t.Fatal("a cross-site frame loaded inside the admitted profile")
	}
}

// ---------------------------------------------------------------------------
// Test 2 — a POST does not leave, and the same POST leaves with the fence off.
// ---------------------------------------------------------------------------

func TestAFormSubmissionDoesNotLeave(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/form"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Send")

	if _, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: button}); err != nil {
		t.Fatalf("act: %v", err)
	}
	if site.reached("POST /submit", settle) {
		t.Fatal("the POST left the machine")
	}
	// There is deliberately NO assertion here about what the agent was told, and the absence is a
	// measurement rather than a gap. This test used to require the refusal to arrive on the click or
	// the act after it, and failed about one run in three; the comment blamed the settle window.
	// MEASURED, 2026-08-16, against the pinned build: twelve consecutive acts over 16.5s never
	// produced one either, so the window was never the reason. A form POST is stopped TWICE — by the
	// method rule here, and by `form-action 'none'` in the CSP `fence.Directives` injects — and the
	// two race inside Chrome. When the CSP wins, the renderer abandons the submission before a
	// request exists, so Fetch never pauses and the fence has nothing to report. That is the
	// STRONGER of the two outcomes, and demanding a message would have been demanding the weaker one
	// win. The reporting half of §6.2 is proved next door, on a channel only the fence stops.

	// Control: the same form, same server, fence off.
	conn := control(t)
	page := openIn(t, conn, site.origin()+"/form")
	time.Sleep(500 * time.Millisecond)
	evaluate(t, conn, page, `document.getElementById('f').submit()`)
	if !site.reached("POST /submit", settle) {
		t.Fatal("the POST did not arrive with the fence off either; the test proves nothing")
	}
}

// ---------------------------------------------------------------------------
// Test 2b — the act that caused a refusal is the one that reports it.
// ---------------------------------------------------------------------------

// TestTheActThatCausedARefusalIsToldAboutIt is the second half of spec §6.2 — "o act que o causou
// responde ao agente" — and the only place in this package that measures it against a real browser.
//
// It is a LINK and not the form above for a reason that took a measurement to find. Every other
// channel in this file is stopped by two things at once: the fence, and a directive in the CSP the
// fence injects. Two stops are what defence in depth is for, but they race, and a test that requires
// a message can only pass when the half that produces one wins. A top-level navigation is the single
// channel `fence.Directives` says nothing about — there is no `navigate-to` in it — so the fence's
// own rule is the only thing standing here, and "the agent was told" becomes a fact rather than a
// coin toss. MEASURED: six runs out of six, against the pinned build.
//
// What must never happen is the refusal being LOST, so the second act exists: a refusal that lands
// after the settle window is carried on the session's cursor (see chrome.session.reportedUpTo), and
// arriving late is the documented behaviour. Arriving never is the failure.
func TestTheActThatCausedARefusalIsToldAboutIt(t *testing.T) {
	admitted := newSite(t)
	stranger := newSite(t)

	driver, _ := fenced(t, admitting(admitted))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: admitted.origin() + "/link?href=" + stranger.origin() + "/page",
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	link := findRef(t, snapshot, "Go")

	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: link})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if stranger.reached("GET /page", settle) {
		t.Fatal("the navigation reached the stranger")
	}
	if result.Outcome != browser.OutcomeRefused {
		second, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionScroll, Ref: link})
		if err != nil {
			t.Fatalf("second act: %v", err)
		}
		if second.Outcome != browser.OutcomeRefused {
			t.Fatalf("neither act told the agent the fence stopped anything: %+v then %+v", result, second)
		}
		result = second
	}
	// The reason and not just the fact. An agent told "refused" with no reason retries the same
	// click, which is the loop §6.2's second half exists to break.
	//
	// `loopback` and not `off-allowlist`, which is a property of the harness rather than of the
	// rule: every site here is a 127.0.0.1 server, so a stranger is one of THIS MACHINE's own
	// services, and that is the more specific of the two reasons — see browser.ConsequenceLoopback,
	// which is kept separate precisely because it is the refusal aimed at us. Both are the fence and
	// neither is the CSP, which is all this test needs to be about.
	if result.Refusal == nil || result.Refusal.Consequence != browser.ConsequenceLoopback {
		t.Fatalf("refused for the wrong reason: %+v", result.Refusal)
	}

	// Control: the same page, the same click, pointed somewhere the profile admits. Without it,
	// "refused" would also be what a click that resolved to the wrong node produced, and the test
	// would pass while proving that the browser cannot navigate at all.
	allowed, err := driver.Open(ctx, browser.OpenRequest{
		URL: admitted.origin() + "/link?href=" + admitted.origin() + "/page",
	})
	if err != nil {
		t.Fatalf("open the control page: %v", err)
	}
	controlSnapshot, err := driver.Snapshot(ctx, allowed.ID, false)
	if err != nil {
		t.Fatalf("snapshot the control page: %v", err)
	}
	if _, err := driver.Act(ctx, allowed.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  findRef(t, controlSnapshot, "Go"),
	}); err != nil {
		t.Fatalf("control act: %v", err)
	}
	if !admitted.reached("GET /page", settle) {
		t.Fatal("the admitted link did not navigate either; the click never worked")
	}
}

// ---------------------------------------------------------------------------
// Test 2c — the hole that is no longer one. Spec §6.2b.
// ---------------------------------------------------------------------------

// TestWebRTCUDPDoesNotLeaveTheFence closes spec §6.2b, which was open from the spike until
// 2026-08-19 and is the reason this test used to assert the opposite.
//
// # What closed it, after six things did not
//
// No command line does. The spike tried CSP `webrtc 'block'`, --disable-webrtc,
// --disable-features=WebRtc, --disable-blink-features=RTCPeerConnection, deleting the global per
// document, and --force-webrtc-ip-handling-policy=disable_non_proxied_udp; two of those were
// re-measured here against the pinned build and still leaked. The seventh mechanism is not a flag:
// `WebRtcIPHandlingPolicy` maps to an ordinary profile preference, and the profile is ours to write.
// See launch.applyWebRTCPolicy.
//
// # Both controls are still load-bearing, and now more than before
//
// "No packet arrived" is what a closed hole looks like. It is ALSO what a page whose script never
// ran looks like, and what a sink bound to the wrong socket looks like — and now that this test
// PASSES on silence, a broken test reads as a security guarantee rather than as a failure. That is
// the worst direction for a mistake to point, so the sink is proved to receive before the browser
// exists, and the page is proved to have reached setLocalDescription before its silence is allowed
// to mean anything.
func TestWebRTCUDPDoesNotLeaveTheFence(t *testing.T) {
	sink := newUDPSink(t)
	site := newSite(t)

	// Control one: the sink receives. Sent from this process, before a browser exists.
	probe, err := net.Dial("udp", sink.addr())
	if err != nil {
		t.Fatalf("dialling the sink: %v", err)
	}
	if _, err := probe.Write([]byte("probe")); err != nil {
		t.Fatalf("probing the sink: %v", err)
	}
	_ = probe.Close()
	if !sink.gotPacket(settle) {
		t.Fatal("the sink did not hear a packet this process sent it; it would not hear the browser either")
	}

	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/webrtc?stun=" + sink.addr(),
	}); err != nil {
		t.Fatalf("open: %v", err)
	}

	// Control two: the page ran and ICE was actually started. The beacon is an image GET, which the
	// fence allows and the CSP does not cover — see fence.Directives, which deliberately omits
	// img-src.
	if !site.reached("BEACON ran", settle) {
		t.Fatal("the page's script never executed; this test measured nothing")
	}
	if !site.reached("BEACON offer", settle) {
		t.Fatal("the script ran but never reached setLocalDescription; this test measured nothing")
	}

	if sink.gotPacket(settle) {
		t.Fatal("UDP left the fenced browser for an address the page chose: spec §6.2b is open again. " +
			"Check that launch.applyWebRTCPolicy still writes webrtc.ip_handling_policy into the " +
			"profile, and that this Chromium revision still honours it — nothing on the command line " +
			"does, so if the preference stopped working there is no fallback in place.")
	}
}

// ---------------------------------------------------------------------------
// Test 3 — a WebSocket handshake does not complete.
// ---------------------------------------------------------------------------

func TestAWebSocketDoesNotConnect(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/ws"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if site.reached("UPGRADE /socket", settle) {
		t.Fatal("the handshake reached the origin")
	}

	// Control: the same page with no fence. This is the one channel CDP is blind to — the spike
	// measured Fetch never seeing a ws: url and setBlockedURLs completing the handshake regardless —
	// so without a control this test would be indistinguishable from a page that never ran.
	conn := control(t)
	openIn(t, conn, site.origin()+"/ws")
	if !site.reached("UPGRADE /socket", settle) {
		t.Fatal("the handshake did not arrive with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 4 — window.open does not produce a page.
// ---------------------------------------------------------------------------

func TestAPopupDoesNotOpen(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/popup"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("BEACON popup-null", settle) {
		t.Fatal("window.open did not return null")
	}
	if site.reached("GET /opened", settle) {
		t.Fatal("the popup navigated")
	}

	conn := control(t)
	openIn(t, conn, site.origin()+"/popup")
	if !site.reached("GET /opened", settle) {
		t.Fatal("the popup did not open with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 5 — a blob: document navigates, and nothing leaves it.
// ---------------------------------------------------------------------------

// TestABlobDocumentIsContainedAndNotClosed is deliberately not called "blocked".
//
// The spike measured that blob: and javascript: DO navigate and that Fetch sees nothing at all; what
// contains them is that the blob document INHERITS its parent's CSP. So the claim under test is the
// only one that held: the document loads, and nothing gets out of it.
func TestABlobDocumentIsContainedAndNotClosed(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/blob"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("BEACON blob-loaded", settle) {
		t.Fatal("the blob document did not load; this test asserts containment, not blocking")
	}
	if site.reached("BEACON blob-fetch-escaped", settle) {
		t.Fatal("a fetch escaped the blob document; the parent's CSP was not inherited")
	}
}

// ---------------------------------------------------------------------------
// Test 6 — a service worker does not install.
// ---------------------------------------------------------------------------

func TestAServiceWorkerDoesNotInstall(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/sw"}); err != nil {
		t.Fatalf("open: %v", err)
	}
	if site.reached("GET /sw.js", settle) {
		t.Fatal("the origin served the worker script; the registration would have installed")
	}
	if site.reached("BEACON sw-registered", settle) {
		t.Fatal("register() resolved; a worker installed in the profile")
	}

	// Control, and it is the one the spike's §5.8 table is about: with the fence off the script IS
	// served and the worker installs. On the PAGE session it also installed — which is why the fence
	// lives on the browser session.
	conn := control(t)
	openIn(t, conn, site.origin()+"/sw")
	if !site.reached("GET /sw.js", settle) {
		t.Fatal("the script was not served with the fence off either; the page never ran")
	}
}

// ---------------------------------------------------------------------------
// Test 8 — a download does not write to disk.
// ---------------------------------------------------------------------------

func TestADownloadDoesNotReachTheDisk(t *testing.T) {
	site := newSite(t)
	driver, profile := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/download"})
	if err != nil && session.Refusal == nil {
		// A refused navigation is the expected shape; a transport error is not.
		if !strings.Contains(err.Error(), "ERR_") {
			t.Fatalf("open: %v", err)
		}
	}
	if found := findOnDisk(t, profile, "report.txt"); found != "" {
		t.Fatalf("the download was written to %s", found)
	}
}

// ---------------------------------------------------------------------------
// Test 7 — failing closed.
// ---------------------------------------------------------------------------

// TestAnUnarmableFenceProducesNoDriver is test 7's real-browser half.
//
// The "Fetch.enable is unavailable" case cannot be staged against a real Chrome — it is available —
// so that half is proven in chrome's unit tests against a browser that refuses the call. What CAN be
// staged here is the other precondition: a driver refuses to exist without a policy, and therefore
// without one nothing navigates at all.
func TestAnUnarmableFenceProducesNoDriver(t *testing.T) {
	site := newSite(t)
	proxy, err := fence.NewProxy(admitting(site))
	if err != nil {
		t.Fatalf("proxy: %v", err)
	}
	defer proxy.Close()

	if _, err := fence.NewProxy(fence.Policy{}); err == nil {
		t.Fatal("a proxy started with no policy")
	}
	// And the browser-side half: see chrome.TestAFenceWithNoPolicyRefusesToProduceADriver, which
	// asserts the browser is not touched at all before the policy is checked.
}

func drain(s *site) {
	for {
		select {
		case <-s.arrived:
		default:
			return
		}
	}
}

func findRef(t *testing.T, snapshot browser.Snapshot, name string) string {
	t.Helper()
	for _, element := range snapshot.Elements {
		if strings.Contains(element.Name, name) {
			return element.Ref
		}
	}
	t.Fatalf("no element named %q in the snapshot: %+v", name, snapshot.Elements)
	return ""
}

// ---------------------------------------------------------------------------
// Test 9 — what the agent actually receives.
// ---------------------------------------------------------------------------

// TestASnapshotCarriesTheProseAndTheStateOfWhatItShows.
//
// The unit tests around `collect` feed it accessibility nodes this repository wrote. This one feeds
// it a page CHROMIUM read, which is the only way to know that the roles, the value and the checked
// property arrive in the shape the filter expects — an accessibility tree is Chromium's opinion
// about ordinary HTML, and the version of that opinion in a test author's head is not evidence.
//
// Until 2026-08-19 a snapshot carried controls and headings and nothing else, so the agent could
// operate a page it could not read: `example.com` came back as one heading and one link, with the
// paragraph absent. That gap is what the prose half of this asserts.
func TestASnapshotCarriesTheProseAndTheStateOfWhatItShows(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/reading"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	find := func(name string) *browser.Element {
		for i := range snapshot.Elements {
			if snapshot.Elements[i].Name == name {
				return &snapshot.Elements[i]
			}
		}
		return nil
	}
	has := func(element *browser.Element, want string) bool {
		if element == nil {
			return false
		}
		for _, state := range element.State {
			if state == want {
				return true
			}
		}
		return false
	}

	// The prose.
	var prose string
	for _, element := range snapshot.Elements {
		if element.Role == "text" {
			prose += element.Name + "\n"
		}
	}
	if !strings.Contains(prose, "Revenue fell by eleven percent") {
		t.Errorf("the page's own words are missing; the agent can operate this page but not read it.\nprose was: %q", prose)
	}

	// Not said twice. A control's name comes from its StaticText child, and emitting both is how a
	// snapshot doubles in size on a page that is mostly links.
	if strings.Count(prose, "Continue") > 0 {
		t.Errorf("a button's own label came back again as prose: %q", prose)
	}

	// The state.
	if box := find("Email"); box == nil || box.Value != "someone@example.org" {
		t.Errorf("a textbox did not report what is in it: %+v", box)
	}
	if !has(find("Remember me"), "checked") {
		t.Errorf("a ticked checkbox did not say so: %+v", find("Remember me"))
	}
	if !has(find("Send updates"), "unchecked") {
		t.Errorf("an unticked checkbox did not say so, which is indistinguishable from having no state: %+v", find("Send updates"))
	}
	if !has(find("Not yet"), "disabled") {
		t.Errorf("a dead button did not say so; the agent will press it forever: %+v", find("Not yet"))
	}
	if has(find("Continue"), "disabled") {
		t.Errorf("a live button was reported dead: %+v", find("Continue"))
	}
}
