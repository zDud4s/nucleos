//go:build browsergate

package gate_test

import (
	"context"
	"encoding/json"
	"fmt"
	"html/template"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/chrome"
	"nucleosbrowser/fence"
	"nucleosbrowser/launch"
)

// chromium finds a browser to drive, or skips with a reason a person can act on.
//
// Skipping rather than failing is spec §11's rule for this group: a machine without the binary
// downloaded is not a broken repository, and a group that failed there would train everyone to
// ignore it. It is the neighbour of the CLAUDE.md note about echo.exe and PATH.
func chromium(t *testing.T) string {
	t.Helper()
	if fromEnv := os.Getenv("NUCLEOS_BROWSER_CHROMIUM"); fromEnv != "" {
		if _, err := os.Stat(fromEnv); err == nil {
			return fromEnv
		}
		t.Skipf("NUCLEOS_BROWSER_CHROMIUM points at %s, which is not there", fromEnv)
	}
	// The pinned install first: it is what production runs, and a gate that silently preferred the
	// system browser would be proving things about a version nobody ships.
	//
	// Asked of `launch.Install` rather than globbed. An earlier version built the path by hand as
	// `chromium-*/chrome.exe` and was one directory short — the archive unpacks to
	// `chromium-<rev>/chrome-win/chrome.exe` — so it never matched, and this fell through to the
	// system Chrome every time WITHOUT SAYING SO. That is why every result this group produced before
	// today was about a browser nobody ships. One source of truth for the path is the fix, and
	// `TestTheGateRunsAgainstThePinnedBuild` is what stops it drifting back.
	if install := (launch.Install{Root: installRoot(t), Pin: launch.DefaultPin()}); install.Present() {
		return install.ExecutablePath()
	}
	for _, candidate := range systemCandidates() {
		if _, err := os.Stat(candidate); err == nil {
			return candidate
		}
	}
	t.Skip("no chromium: install the pinned build, or set NUCLEOS_BROWSER_CHROMIUM to a chrome executable")
	return ""
}

func systemCandidates() []string {
	switch runtime.GOOS {
	case "windows":
		return []string{
			`C:\Program Files\Google\Chrome\Application\chrome.exe`,
			`C:\Program Files (x86)\Google\Chrome\Application\chrome.exe`,
		}
	case "darwin":
		return []string{"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
	default:
		return []string{"/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser"}
	}
}

// site is a local server with pages we wrote. Never the live internet — §11.
//
// Every handler records what ARRIVED. That is what separates "the fence blocked it" from "the
// request was never made", and it is the difference three retracted spike results turned on.
type site struct {
	*httptest.Server
	arrived chan string
}

func newSite(t *testing.T) *site {
	t.Helper()
	s := &site{arrived: make(chan string, 128)}
	mux := http.NewServeMux()

	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>%s</title><h1 id=here>%s</h1>`, r.URL.Path, r.URL.Path)
	})
	mux.HandleFunc("/form", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>form</title>
			<form id=f method=post action="/submit"><button id=go type=submit>Send</button></form>`)
	})
	mux.HandleFunc("/submit", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		fmt.Fprint(w, "ok")
	})
	// A menu that opens on pointerdown and on nothing else, which is how a great many real components
	// are built: it is what makes them feel immediate. Under a click synthesised by calling
	// element.click() this menu never opened, and the act still said done.
	mux.HandleFunc("/pointer", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>pointer</title><body>
			<h1>Messages</h1>
			<button id=go>Actions</button>
			<div id=menu></div>
			<script>
			document.getElementById('go').addEventListener('pointerdown', () => {
				document.getElementById('menu').innerHTML = '<button id=item>Archive</button>';
			});
			</script>`)
	})

	// A menu that opens on hover and on nothing else. There is no hover verb, and this page is the
	// reason one is not needed: moving the pointer onto the parent before pressing it is part of
	// clicking it, so the submenu is open by the time the next reading is taken.
	mux.HandleFunc("/hover", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>hover</title><body>
			<h1>Documents</h1>
			<button id=parent>File</button>
			<div id=menu></div>
			<script>
			document.getElementById('parent').addEventListener('mouseover', () => {
				document.getElementById('menu').innerHTML = '<button id=item>Export</button>';
			});
			</script>`)
	})

	// A button under a consent banner. The accessibility tree carries the button either way — an
	// overlay is a painting decision and the tree is not about painting — so the reading shows a
	// button the agent cannot actually reach.
	mux.HandleFunc("/covered", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>covered</title><body>
			<h1>Settings</h1>
			<button id=go onclick="document.title = 'saved'">Save</button>
			<div id=banner style="position:fixed; inset:0; background:rgba(0,0,0,0.6); color:white">
				We use cookies
			</div>`)
	})

	// A 404 that looks like a page, because that is what a 404 IS. A heading, a sentence, a search
	// box — nothing about the reading of it says the request failed, which is the entire reason the
	// status has to be carried separately.
	mux.HandleFunc("/missing", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.WriteHeader(http.StatusNotFound)
		fmt.Fprint(w, `<!doctype html><title>Not found</title><body>
			<h1>We could not find that</h1>
			<p>Try searching for it instead.</p>
			<label>Search <input id=q name=q></label>`)
	})

	// A page that asks the PERSON a question, which is a thing the agent is not. confirm() blocks the
	// renderer until somebody answers the dialog, and with Page.enable on (chrome/driver.go) that
	// somebody has to be this driver: Chromium hands the dialog to the attached client and waits.
	//
	// The title says which way it was answered, because "the dialog went away" is not the fact worth
	// measuring — WHICH answer the page received is, and a wrong one confirms deletions.
	mux.HandleFunc("/dialog", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>dialog</title><body>
			<h1>Settings</h1>
			<button id=go onclick="document.title = confirm('Delete everything?') ? 'accepted' : 'dismissed'">Delete</button>`)
	})
	// The other half of the same rule: a search box. Same page shape as /form, same button, and the
	// only difference is the one the fence is supposed to care about. It carries a filled field
	// rather than an empty form because what has to arrive at the server is the FIELD — a submission
	// that navigates to /found and loses the query is a submission in name only.
	mux.HandleFunc("/search", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>search</title>
			<form id=f method=get action="/found">
				<label>Query <input id=q name=q value="invoices"></label>
				<button id=go type=submit>Search</button>
			</form>`)
	})
	mux.HandleFunc("/found", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>found</title><h1>Results for %s</h1>`,
			template.HTMLEscapeString(r.URL.Query().Get("q")))
	})
	// ---- writing ---------------------------------------------------------------------------
	//
	// The pages the write rule is measured against. Every one of them is a form that would be
	// perfectly ordinary on a real site — a reply box, a page that saves itself, a form aimed
	// somewhere else — and the only thing separating them is which of the five conditions holds.

	// A reply form. The password field is not decoration: it is what the record has to name and must
	// never carry, and a form with one is the ordinary case rather than an exotic one.
	mux.HandleFunc("/write", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>write</title>
			<form id=f method=post action="/wrote">
				<label>Body <input id=body name=body value="looks fine to me"></label>
				<label>Secret <input type=password name=secret value="hunter2"></label>
				<input type=hidden name=csrf value="t0ken">
				<button id=go type=submit>Send reply</button>
			</form>`)
	})
	mux.HandleFunc("/wrote", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>wrote</title><h1 id=here>Reply delivered</h1>`)
	})

	// The same form, submitted by the PAGE and not by anybody acting on it. This is what hostile
	// content inside an origin the person granted looks like from the fence's side, and it is the
	// only reason the grant is not the whole rule.
	mux.HandleFunc("/selfwrite", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>selfwrite</title><h1 id=here>selfwrite</h1>
			<form id=f method=post action="/wrote"><input name=body value="x"></form>
			<script>setTimeout(() => document.getElementById('f').submit(), 50);</script>`)
	})

	// A form on this origin aimed at another one. The exfiltration shape §6.2 exists to close, and
	// the one no grant turns into something else.
	mux.HandleFunc("/crosswrite", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>crosswrite</title>
			<form id=f method=post action=%q>
				<input name=body value="everything I just read">
				<button id=go type=submit>Send reply</button>
			</form>`, r.URL.Query().Get("to"))
	})

	// One act, two forms. What makes the window a window and not a switch: the click submits the form
	// the button belongs to, and that form's own handler slips a second submission through behind it.
	//
	// Both target iframes, so neither navigates the page away and both are observable. The button is
	// INSIDE the first form and is a real submit button, because a button sitting outside every form
	// arms nothing at all — which is a different rule, tested elsewhere, and would make this page
	// measure that one instead.
	mux.HandleFunc("/twowrites", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>twowrites</title>
			<form id=a method=post action="/wrote" target=one>
				<input name=body value="one">
				<button id=go type=submit>Send reply</button>
			</form>
			<form id=b method=post action="/alsowrote" target=two><input name=body value="two"></form>
			<iframe name=one></iframe><iframe name=two></iframe>
			<script>
				document.getElementById('a').addEventListener('submit', () => {
					document.getElementById('b').submit();
				});
			</script>`)
	})
	mux.HandleFunc("/alsowrote", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		fmt.Fprint(w, "also")
	})

	// A search that posts. The shape of every chat box and half the search boxes on the web: one
	// field, no visible button, and Enter is how it is sent.
	mux.HandleFunc("/keywrite", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>keywrite</title>
			<form id=f method=post action="/wrote">
				<label>Message <input id=q name=message value="on my way"></label>
			</form>`)
	})

	// A POST from a script rather than from a form. The path an injection takes without passing
	// through any act at all, and the reason the ferry stayed GET-only.
	mux.HandleFunc("/fetchwrite", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>fetchwrite</title><h1 id=here>fetchwrite</h1>
			<button id=go>Send reply</button>
			<script>
				document.getElementById('go').addEventListener('click', () => {
					fetch('/wrote', {method: 'POST', body: 'body=x'}).catch(() => {});
				});
			</script>`)
	})

	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Disposition", `attachment; filename="report.txt"`)
		w.Header().Set("Content-Type", "text/plain")
		fmt.Fprint(w, "this should never reach the disk")
	})
	mux.HandleFunc("/sw.js", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/javascript")
		fmt.Fprint(w, `self.addEventListener('install', () => self.skipWaiting());`)
	})

	// Pages report back with an image, not with fetch.
	//
	// The fence's CSP is connect-src 'none', so a page inside it cannot fetch, XHR or beacon — which
	// is the whole point. img-src is deliberately not governed (see fence.Directives), so an <img>
	// is the one channel a fenced page still has, and it is what these pages use to say what
	// happened to them. Without it every test here could only observe silence.
	// A page that frames whatever the query string says. Spec §5.4: an iframe is a document that is
	// not top level, which is the gap the earlier wording left open.
	mux.HandleFunc("/framing", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>framing</title><iframe src=%q></iframe>`,
			r.URL.Query().Get("src"))
	})

	// A page that points an RTCPeerConnection at whatever STUN address the query string names, and
	// then does nothing else. Naming the address from outside is what makes this a measurement: the
	// page chooses the destination, which is exactly the shape spec §6.2b describes.
	//
	// A data channel and no media, deliberately. Microphone and camera need a permission the fenced
	// profile never grants; a data channel needs none, so this is the path that is actually open. The
	// offer is what makes Chrome start ICE and send the first STUN binding request over UDP.
	mux.HandleFunc("/webrtc", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>webrtc</title><h1 id=here>webrtc</h1><script>
			const say = what => { new Image().src = "/beacon?what=" + what; };
			say("ran");
			try {
				const pc = new RTCPeerConnection({iceServers: [{urls: "stun:%s"}]});
				pc.createDataChannel("gate");
				pc.createOffer()
					.then(offer => pc.setLocalDescription(offer))
					.then(() => say("offer"))
					.catch(() => say("rejected"));
			} catch (e) {
				say("threw");
			}
		</script>`, r.URL.Query().Get("stun"))
	})

	// A page with one link to wherever the query string says. It exists for the reporting half of
	// spec §6.2 and not for the blocking half, and it was a link and not the form above because the
	// injected CSP carried `form-action 'none'`, so a form POST was stopped twice over and the two
	// stops raced. That is no longer true — the directive admits http: and https: now, and a
	// same-origin POST meets the method rule alone — so the form would serve here too. It stays a
	// link because nothing in `fence.Directives` bounds a top-level navigation at all (there is no
	// `navigate-to` in it), which makes this the one channel with a single mechanism standing by
	// construction rather than by the current value of a directive.
	// A page with prose, a filled box and a ticked control, for the snapshot group. It is deliberately
	// ordinary HTML with no ARIA: what matters is what Chromium's own accessibility tree makes of a
	// page nobody wrote for a machine, which is every page the agent will actually meet.
	mux.HandleFunc("/reading", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>reading</title>
			<h1>Quarterly report</h1>
			<p>Revenue fell by eleven percent, which nobody had forecast.</p>
			<label>Email <input id=email type=text value="someone@example.org"></label>
			<label><input id=tick type=checkbox checked> Remember me</label>
			<label><input id=untick type=checkbox> Send updates</label>
			<button id=go>Continue</button>
			<button id=dead disabled>Not yet</button>`)
	})

	mux.HandleFunc("/link", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>link</title><a id=go href=%q>Go</a>`,
			r.URL.Query().Get("href"))
	})

	// A page that arrives empty and fills itself from an API, which is most of the web this pillar
	// exists to reach. `connect-src 'none'` closes the fetch, so what renders is a shell — and a
	// shell is a correct reading of an empty page, which is why nothing used to contradict it.
	mux.HandleFunc("/spa", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		source := r.URL.Query().Get("src")
		if source == "" {
			source = "/content"
		}
		fmt.Fprintf(w, `<!doctype html><title>spa</title><body>
			<h1>Dashboard</h1>
			<div id=app></div>
			<script>
			const SRC = %q;
			window.addEventListener('load', () => {
				fetch(SRC).then(r => r.text())
					.then(t => { document.getElementById('app').innerHTML = t; })
					.catch(e => { new Image().src = '/beacon?what=fetch-failed'; });
			});
			</script>`, source)
	})
	// The SPA's other half, and the one the ferry is actually used through: a page that fetches when
	// somebody presses something rather than when it loads. This does not navigate, so nothing in the
	// load path is watching, and the fetch is started from a timeout rather than straight out of the
	// handler because that is what a framework does — a version of this that called fetch inline
	// would pass on the ordering of one CDP round trip and prove nothing about the wait.
	mux.HandleFunc("/click-spa", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>click-spa</title><body>
			<h1>Reports</h1>
			<button id=load>Load the report</button>
			<button id=inert>Do nothing</button>
			<div id=app></div>
			<script>
			document.getElementById('load').addEventListener('click', () => {
				setTimeout(() => {
					fetch('/slow-content').then(r => r.text())
						.then(t => { document.getElementById('app').innerHTML = t; })
						.catch(e => { new Image().src = '/beacon?what=click-fetch-failed'; });
				}, 30);
			});
			</script>`)
	})
	// Two links with the same words and different destinations, which is what a directory looks
	// like. Plus one that leaves the host, because that is the case where the whole address is the
	// news rather than the path.
	mux.HandleFunc("/links", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, PAGE_LINKS, otherHostOf(r))
	})

	// A box that has the keyboard from the moment the page loads. `press` with no ref goes wherever
	// focus is, so this is the page that says whether the reading can name it.
	mux.HandleFunc("/focus", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, PAGE_FOCUS)
	})

	// A page whose content is drawn, not written: nothing of it reaches the accessibility tree. A
	// chart, a map, a PDF viewer. The heading is there so the reading is not empty — an empty one
	// would be ambiguous with a page that failed to load, and the claim is about a page that
	// loaded fine and still cannot be read.
	mux.HandleFunc("/canvas", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, PAGE_CANVAS)
	})

	// The two halves of the look-at-a-cross-site-frame measurement, and they are laid out with
	// absolute coordinates on purpose: the assertion is about WHERE a label lands, so the test has to
	// know where the thing being labelled is without asking the browser — asking would mean asking
	// the same process boundary the measurement is about.
	mux.HandleFunc("/lookframe", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, PAGE_LOOKFRAME, r.URL.Query().Get("src"))
	})
	mux.HandleFunc("/lookbutton", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, PAGE_LOOKBUTTON)
	})

	// A page that can be asked, from inside itself, whether the look left anything behind. The
	// question has to be answered by the DOM rather than by a snapshot, because the overlay is
	// aria-hidden — so a snapshot would report a clean page whether or not one was still there,
	// which is exactly the failure that would go unnoticed.
	mux.HandleFunc("/lookclean", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, PAGE_LOOKCLEAN)
	})

	// An endless list: scrolling is what loads more, which is the case that made excluding scroll
	// from the wait wrong. Two steps for the same reason as /click-render — the first inside the
	// reaction window, the last well outside it.
	mux.HandleFunc("/endless", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, PAGE_ENDLESS)
	})

	// A click that changes the page and asks for NOTHING, which is the half the ferry cannot see: no
	// request, no navigation, just the page redrawing itself. A menu opening, a route rendering from
	// data already in memory, a list filtering. The delay is short and real — a framework does not
	// paint inside the click handler.
	mux.HandleFunc("/click-render", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>click-render</title><body>
			<h1>Filters</h1>
			<button id=show>Show the detail</button>
			<div id=app></div>
			<script>
			// In two steps, and the timings are the whole point. The first lands INSIDE the reaction
			// window, so the wait has something to see; the last lands well outside it, so a wait
			// that only counted the window would return with the placeholder on screen and the test
			// would pass without measuring anything.
			document.getElementById('show').addEventListener('click', () => {
				const app = document.getElementById('app');
				setTimeout(() => { app.innerHTML = '<p>Loading the detail...</p>'; }, 80);
				setTimeout(() => {
					app.innerHTML =
						'<p>Revenue fell by eleven percent, which nobody had forecast.</p>';
				}, 450);
			});
			</script>`)
	})
	mux.HandleFunc("/content", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<p>Revenue fell by eleven percent, which nobody had forecast.</p>
			<button id=ok>Approve the write-down</button>`)
	})

	// A server that answers and then does not stop, which is how most chat and progress APIs work.
	// It declares itself, which is what lets the ferry refuse it at the headers instead of thirty
	// seconds later — and the handler holds the connection open so that a version which read the
	// body anyway would be measured doing it.
	mux.HandleFunc("/stream", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		if flusher, ok := w.(http.Flusher); ok {
			fmt.Fprint(w, "data: one\n\n")
			flusher.Flush()
		}
		<-r.Context().Done()
	})

	// The same content as /content, and slow on purpose, for the tests whose claim is about WAITING
	// rather than about reading. An act already spends up to a second and a half waiting for a fence
	// refusal, so anything a page finishes inside that window is covered whether the wait after it
	// works or not — and a test written against such a page passes either way.
	mux.HandleFunc("/slow-content", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		time.Sleep(2500 * time.Millisecond)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, "<p>Revenue fell by eleven percent, which nobody had forecast.</p>"+
			"<button id=ok>Approve the write-down</button>")
	})

	// The rows an endless list loads, and slowly on purpose: longer than the window an act already
	// spends waiting for a fence refusal. A wait that did not drain what the ferry is carrying returns
	// before this answers, and the reading shows the list exactly as it was.
	mux.HandleFunc("/slow-rows", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		time.Sleep(2500 * time.Millisecond)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, "<p>Revenue fell by eleven percent, which nobody had forecast.</p>")
	})

	// A neighbouring service, which is what the modern web actually looks like: the page is
	// app.example.com and its data is at api.example.com. What it answers with is decided by the
	// query, because the whole question here is whether the SERVER opted in — the ferry applies the
	// browser's own rule, so a service that says nothing is not read and one that names the page is.
	mux.HandleFunc("/cors", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		if allow := r.URL.Query().Get("allow"); allow != "" {
			w.Header().Set("Access-Control-Allow-Origin", allow)
		}
		if r.URL.Query().Get("credentials") == "true" {
			w.Header().Set("Access-Control-Allow-Credentials", "true")
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<p>The neighbouring service answered.</p>`)
	})

	// A table, because the accessibility tree HAS the grid and the question is only what Chromium
	// calls its parts. Three roles are assumed by the snapshot — row, cell, columnheader — and an
	// assumption about role names is exactly the kind this repository has got wrong three times
	// against a hand-built tree and only ever settled here.
	mux.HandleFunc("/table", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>table</title><table>
			<caption>Quarterly</caption>
			<tr><th>Quarter</th><th>Revenue</th></tr>
			<tr><td>Q1</td><td>-11%</td></tr>
			<tr><td><a href="/reading">Q2</a></td><td>+4%</td></tr>
			</table>`)
	})

	// The page the verb group works on.
	//
	// It says out loud what reached it, which is the only way to tell "the key arrived" from "the
	// page did nothing". Typing goes through Input.insertText — what a paste does — so a box that
	// reacts to a keystroke never heard one, and the evidence for that was a page that did not
	// change, indistinguishable from a search with no results.
	mux.HandleFunc("/controls", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>controls</title><body>
			<h1>Controls</h1>
			<label>Search <input id=q type=text></label>
			<label>Where <select id=where>
				<option value=pt>Portugal</option>
				<option value=es>Spain</option>
			</select></label>
			<div style="height: 4000px"></div>
			<script>
			function beacon(what){ new Image().src = '/beacon?what=' + encodeURIComponent(what); }
			document.getElementById('q').addEventListener('keydown', e => beacon('keydown-' + e.key));
			document.getElementById('where').addEventListener('change', e => beacon('chose-' + e.target.value));
			window.addEventListener('scroll', () => { if (window.scrollY > 100) { beacon('scrolled'); } });
			</script>`)
	})

	// Two pages for the profile group. They are about identity rather than the fence: one hands the
	// browser a cookie, the other says which cookie came back — which is how "the profile is the
	// identity" (spec §4.2) becomes something a test can observe from outside the browser.
	mux.HandleFunc("/set-cookie", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		// Persistent, not a session cookie: spec §4.2 measured that a session cookie is exactly the
		// thing that does NOT survive, and a test built on one would measure the browser's memory
		// instead of the profile on disk.
		http.SetCookie(w, &http.Cookie{Name: "gate", Value: "1", Path: "/", MaxAge: 3600})
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>set-cookie</title><h1>set</h1>`)
	})
	mux.HandleFunc("/whoami", func(w http.ResponseWriter, r *http.Request) {
		value := "none"
		if cookie, err := r.Cookie("gate"); err == nil {
			value = cookie.Value
		}
		select {
		case s.arrived <- "WHOAMI " + value:
		default:
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>whoami</title><h1>%s</h1>`, value)
	})

	mux.HandleFunc("/beacon", func(w http.ResponseWriter, r *http.Request) {
		select {
		case s.arrived <- "BEACON " + r.URL.Query().Get("what"):
		default:
		}
		w.Header().Set("Content-Type", "image/gif")
		_, _ = w.Write([]byte("GIF89a"))
	})

	mux.HandleFunc("/ws", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>ws</title><script>
			function beacon(what){ new Image().src = '/beacon?what=' + what; }
			try {
				const s = new WebSocket(%q);
				s.onopen  = () => beacon('ws-open');
				s.onerror = () => beacon('ws-error');
			} catch (e) { beacon('ws-throw'); }
		</script>`, "ws://"+r.Host+"/socket")
	})

	mux.HandleFunc("/socket", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		// Not a real WebSocket server: reaching this handler at all is the failure being tested for,
		// and completing the handshake would only make the failure harder to read.
		w.WriteHeader(http.StatusOK)
	})

	mux.HandleFunc("/sw", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>sw</title><script>
			function beacon(what){ new Image().src = '/beacon?what=' + what; }
			navigator.serviceWorker.register('/sw.js')
				.then(() => beacon('sw-registered'), () => beacon('sw-failed'));
		</script>`)
	})

	mux.HandleFunc("/popup", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, `<!doctype html><title>popup</title><script>
			function beacon(what){ new Image().src = '/beacon?what=' + what; }
			const opened = window.open('/opened', '_blank');
			beacon(opened ? 'popup-opened' : 'popup-null');
		</script>`)
	})

	mux.HandleFunc("/blob", func(w http.ResponseWriter, r *http.Request) {
		s.note(r)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		// The blob document tries to reach the network from inside itself. Spec §6.2 says a blob: is
		// CONTAINED and not closed — it navigates, and the parent's CSP is what stops anything
		// leaving it. This page is that claim, executable.
		// Inside a load handler: an earlier version appended to document.body from a script in the
		// head, where body is still null, and the page threw before it ever made a blob. The gate
		// read that as "the blob did not load", which was true and told us nothing.
		fmt.Fprint(w, `<!doctype html><title>blob</title><body><script>
			function beacon(what){ new Image().src = '/beacon?what=' + what; }
			window.addEventListener('load', () => {
				const html = "<script>fetch('/beacon?what=blob-fetch-escaped')" +
					".then(()=>0,()=>0);<\/script>";
				const url = URL.createObjectURL(new Blob([html], {type:'text/html'}));
				const frame = document.createElement('iframe');
				frame.src = url;
				frame.onload = () => beacon('blob-loaded');
				document.body.appendChild(frame);
			});
		</script>`)
	})

	s.Server = httptest.NewServer(mux)
	t.Cleanup(s.Close)
	return s
}

func (s *site) note(r *http.Request) {
	label := r.Method + " " + r.URL.Path
	if r.Header.Get("Upgrade") != "" {
		label = "UPGRADE " + r.URL.Path
	}
	select {
	case s.arrived <- label:
	default:
	}
}

// arrivals collects everything that reached the server within a window.
//
// The plural of `reached`, and needed where the claim is about a COUNT rather than about one label:
// asking `reached` twice would consume the answer to the second question while looking for the
// first. "Exactly one of these two forms was sent" is that kind of claim, and it is also the only
// race-free way to state it — which of the two wins the window is up to Chromium's ordering, and the
// rule never said which.
func (s *site) arrivals(within time.Duration) []string {
	var seen []string
	deadline := time.After(within)
	for {
		select {
		case got := <-s.arrived:
			seen = append(seen, got)
		case <-deadline:
			return seen
		}
	}
}

// reached reports whether a label arrived within the window. A window and not an instant, because a
// page's request is not synchronous with the act that caused it.
func (s *site) reached(label string, within time.Duration) bool {
	deadline := time.After(within)
	for {
		select {
		case got := <-s.arrived:
			if got == label {
				return true
			}
		case <-deadline:
			return false
		}
	}
}

func (s *site) origin() string { return s.URL }

// udpSink is a UDP socket that records that something arrived, and nothing else.
//
// It exists because UDP is the one egress path in this package that no HTTP server can observe.
// Every other test here proves a negative by watching a *site — but WebRTC never speaks HTTP, so a
// packet that leaves is invisible to all of them. This is the only listener in the group that would
// notice.
//
// It does not answer. A real STUN server would reply and let ICE proceed; that would measure whether
// a connection can be ESTABLISHED, which is a larger claim than the one that matters. The claim that
// matters is that a byte chosen by the page reached an address chosen by the page, and the first
// binding request already settles it.
type udpSink struct {
	conn    *net.UDPConn
	arrived chan int
}

func newUDPSink(t *testing.T) *udpSink {
	t.Helper()
	conn, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("udp sink: %v", err)
	}
	sink := &udpSink{conn: conn, arrived: make(chan int, 32)}
	go func() {
		buf := make([]byte, 2048)
		for {
			n, _, err := conn.ReadFromUDP(buf)
			if err != nil {
				return
			}
			select {
			case sink.arrived <- n:
			default:
			}
		}
	}()
	t.Cleanup(func() { _ = conn.Close() })
	return sink
}

func (u *udpSink) addr() string { return u.conn.LocalAddr().String() }

func (u *udpSink) gotPacket(within time.Duration) bool {
	select {
	case <-u.arrived:
		return true
	case <-time.After(within):
		return false
	}
}

// admitting is the policy a gate test runs under: a project profile whose only local admission is
// this site. Everything else on loopback — including the browser's own debugging port — stays shut.
// admittingBoth is a profile with business at two addresses, which is the ordinary case for anything
// with an API on its own host. Both go in Loopback, because the gate's sites are http on 127.0.0.1
// and Origins is https-only by design (see fence.Policy).
func admittingBoth(a, b *site) fence.Policy {
	return fence.Policy{
		Profile:  fence.Project,
		Origins:  []string{"https://nucleos.invalid"},
		Loopback: []string{a.origin(), b.origin()},
	}
}

func admitting(s *site) fence.Policy {
	return fence.Policy{
		Profile:  fence.Project,
		Origins:  []string{"https://nucleos.invalid"},
		Loopback: []string{s.origin()},
	}
}

// admittingWritable is `admitting` plus the grant a person gives at the login: this profile may also
// submit forms to that origin.
//
// A separate helper and not a flag, so every test that uses it says in its own first line which of
// the two permissions it is about — and so the pair of tests that differ only in this call is a pair
// a reader can see is a pair.
func admittingWritable(s *site) fence.Policy {
	policy := admitting(s)
	policy.Writable = []string{s.origin()}
	return policy
}

// fenced launches a browser with the fence attached and returns the driver.
//
// A fresh profile every time: spec §5.6 keeps profiles apart, and a shared one would carry a service
// worker from one test into the next — the exact state test 6 exists to detect.
func fenced(t *testing.T, policy fence.Policy) (*chrome.Driver, string) {
	t.Helper()
	proxy, err := fence.NewProxy(policy)
	if err != nil {
		t.Fatalf("proxy: %v", err)
	}
	t.Cleanup(func() { _ = proxy.Close() })

	profile := t.TempDir()
	process, err := launch.Start(context.Background(), launch.Options{
		ExecutablePath: chromium(t),
		ProfileDir:     profile,
		Mode:           browser.ModeAgent,
		ProxyAddr:      proxy.Addr(),
	}, 45*time.Second)
	if err != nil {
		t.Fatalf("launching chromium: %v", err)
	}
	t.Cleanup(process.Stop)

	conn := dial(t, process.Port)
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	driver, err := chrome.Connect(ctx, conn, policy)
	if err != nil {
		t.Fatalf("attaching the fence: %v", err)
	}
	return driver, profile
}

// findOnDisk looks for a file the browser must never have written, and returns where it found it.
//
// It searches the profile AND the person's Downloads folder, because "deny" failing in either place
// is the same failure: spec §11 test 8 is about a byte reaching a disk, not about which disk.
func findOnDisk(t *testing.T, profile, name string) string {
	t.Helper()
	roots := []string{profile}
	if home, err := os.UserHomeDir(); err == nil {
		roots = append(roots, filepath.Join(home, "Downloads"))
	}
	for _, root := range roots {
		var found string
		_ = filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
			if err != nil || entry.IsDir() {
				return nil
			}
			if strings.EqualFold(entry.Name(), name) {
				info, statErr := entry.Info()
				// Only something written during this run. A file the person downloaded last year
				// would otherwise fail the suite for ever.
				if statErr == nil && time.Since(info.ModTime()) < 5*time.Minute {
					found = path
				}
			}
			return nil
		})
		if found != "" {
			return found
		}
	}
	return ""
}

// control launches the SAME browser with the fence off, and drives it over raw CDP.
//
// It builds its own command line instead of calling launch.Args, and that is the point rather than
// duplication: a control that shared the launcher would inherit the launcher's bugs, and the flags
// launch.Args adds in agent mode ARE the fence. What is left here is a headless browser with no
// interception, no proxy and no popup blocking — the "with the fence off" half that §11 requires
// beside each of the eight, after three spike results came back inverted for want of one.
func control(t *testing.T) *cdp.Conn {
	t.Helper()
	profile := t.TempDir()
	marker := filepath.Join(profile, "DevToolsActivePort")
	cmd := exec.Command(chromium(t),
		"--remote-debugging-port=0",
		"--user-data-dir="+profile,
		"--headless=new",
		"--no-first-run",
		"--no-default-browser-check",
		"--disable-background-networking",
		"--disable-search-engine-choice-screen",
		// Chrome's own popup blocker refuses a window.open with no user gesture, in every mode. The
		// control has to be MORE permissive than production or it proves nothing: without this the
		// popup test would pass because Chrome blocked the popup, not because the fence did.
		"--disable-popup-blocking",
	)
	if err := cmd.Start(); err != nil {
		t.Fatalf("starting the control browser: %v", err)
	}
	t.Cleanup(func() {
		if runtime.GOOS == "windows" {
			_ = exec.Command("taskkill", "/T", "/F", "/PID", strconv.Itoa(cmd.Process.Pid)).Run()
		}
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
	})

	deadline := time.Now().Add(45 * time.Second)
	for time.Now().Before(deadline) {
		raw, err := os.ReadFile(marker)
		if err == nil {
			line := strings.SplitN(strings.TrimSpace(string(raw)), "\n", 2)[0]
			if port, err := strconv.Atoi(strings.TrimSpace(line)); err == nil && port > 0 {
				return dial(t, port)
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatal("the control browser never reported a debugging port")
	return nil
}

func dial(t *testing.T, port int) *cdp.Conn {
	t.Helper()
	wsURL, err := launch.DebuggerURL(port, 10*time.Second)
	if err != nil {
		t.Fatalf("finding the debugger url: %v", err)
	}
	conn, err := cdp.Dial(wsURL, 20*time.Second)
	if err != nil {
		t.Fatalf("dialling cdp: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

// openIn navigates a raw connection with no fence on it, and returns the page session.
func openIn(t *testing.T, conn *cdp.Conn, url string) cdp.SessionID {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	created, err := conn.Call(ctx, cdp.BrowserSession, "Target.createTarget", map[string]any{"url": "about:blank"})
	if err != nil {
		t.Fatalf("creating a control target: %v", err)
	}
	var target struct {
		TargetID string `json:"targetId"`
	}
	if err := json.Unmarshal(created, &target); err != nil {
		t.Fatalf("target id: %v", err)
	}
	attached, err := conn.Call(ctx, cdp.BrowserSession, "Target.attachToTarget", map[string]any{
		"targetId": target.TargetID,
		"flatten":  true,
	})
	if err != nil {
		t.Fatalf("attaching to the control target: %v", err)
	}
	var session struct {
		SessionID cdp.SessionID `json:"sessionId"`
	}
	if err := json.Unmarshal(attached, &session); err != nil {
		t.Fatalf("session id: %v", err)
	}
	if _, err := conn.Call(ctx, session.SessionID, "Page.enable", nil); err != nil {
		t.Fatalf("enabling Page on the control: %v", err)
	}
	if _, err := conn.Call(ctx, session.SessionID, "Page.navigate", map[string]any{"url": url}); err != nil {
		t.Fatalf("navigating the control: %v", err)
	}
	return session.SessionID
}

// evaluate runs an expression and returns its value as JSON.
func evaluate(t *testing.T, conn *cdp.Conn, session cdp.SessionID, expression string) json.RawMessage {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	result, err := conn.Call(ctx, session, "Runtime.evaluate", map[string]any{
		"expression":    expression,
		"awaitPromise":  true,
		"returnByValue": true,
	})
	if err != nil {
		t.Fatalf("evaluate: %v", err)
	}
	var payload struct {
		Result struct {
			Value json.RawMessage `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		t.Fatalf("evaluate result: %v", err)
	}
	return payload.Result.Value
}

// otherHostOf is this same server under the name Chromium calls a different site, built from the
// request so the page does not have to be told its own address.
func otherHostOf(r *http.Request) string {
	return "http://localhost:" + portOf(r.Host) + "/reading"
}

func portOf(hostPort string) string {
	if _, port, err := net.SplitHostPort(hostPort); err == nil {
		return port
	}
	return "80"
}

const PAGE_LINKS = `<!doctype html><title>links</title><body>
	<h1>Invoices</h1>
	<ul>
		<li>March <a href="/invoices/1">Details</a></li>
		<li>April <a href="/invoices/2?open=1">Details</a></li>
		<li><a href="%s">Details</a></li>
	</ul>`

const PAGE_FOCUS = `<!doctype html><title>focus</title><body>
	<h1>Search</h1>
	<label>Query <input id=q autofocus></label>
	<label>Notes <input id=n></label>`

const PAGE_CANVAS = `<!doctype html><title>canvas</title><body>
	<h1>Quarterly</h1>
	<canvas id=chart width=600 height=400></canvas>
	<script>
	const ink = document.getElementById('chart').getContext('2d');
	ink.fillStyle = '#333';
	ink.fillRect(20, 20, 120, 300);
	ink.fillText('Revenue fell by eleven percent', 200, 200);
	</script>`

const PAGE_ENDLESS = `<!doctype html><title>endless</title><body>
	<h1>Everything</h1>
	<div id=list><p>Row one.</p></div>
	<div style="height: 4000px"></div>
	<script>
	let loading = false;
	window.addEventListener('scroll', () => {
		if (loading || window.scrollY < 100) { return; }
		loading = true;
		fetch('/slow-rows').then(r => r.text()).then(t => {
			document.getElementById('list').insertAdjacentHTML('beforeend', t);
		});
	});
	</script>`

// PAGE_LOOKFRAME puts a cross-site frame at a known place. margin:0 and border:0 so the numbers in
// the test are the numbers here, with nothing of the browser's own styling in between.
const PAGE_LOOKFRAME = `<!doctype html><title>lookframe</title>
	<style>html,body{margin:0;padding:0;background:#ffffff}</style>
	<body><iframe src=%q style="position:absolute;left:200px;top:150px;width:300px;height:200px;border:0"></iframe>`

// PAGE_LOOKBUTTON is what goes inside it: one button, at a known offset within its own document.
const PAGE_LOOKBUTTON = `<!doctype html><title>lookbutton</title>
	<style>html,body{margin:0;padding:0;background:#ffffff}</style>
	<body><button style="position:absolute;left:20px;top:30px;width:100px;height:40px">Go</button>`

// PAGE_LOOKCLEAN reports its own DOM when the button is pressed, so the test can ask the page
// whether the overlay is still there instead of inferring it from a reading that cannot see one.
const PAGE_LOOKCLEAN = `<!doctype html><title>lookclean</title><body>
	<h1>Clean</h1>
	<p id=out>nobody has asked yet</p>
	<button onclick="out.textContent = 'overlay is ' + (document.getElementById('nucleos-look-overlay') ? 'still here' : 'gone')">Ask</button>`
