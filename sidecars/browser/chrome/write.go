package chrome

import (
	"context"
	"encoding/json"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// The write window: how "an act caused this" becomes something the fence can check.
//
// # The problem this file exists for
//
// The fence sees REQUESTS, and an act is not a request. A form submission arrives at
// Fetch.requestPaused as a POST for a document, and there is nothing in that event to say whether
// the agent pressed Send or whether the page submitted the form by itself while the agent was
// reading it. Those two are the whole difference between the fifth condition of the write rule
// holding and not holding, and without something like this file the grant would be a blank cheque:
// hostile content inside a granted origin — a comment, an issue title, an email body — could submit
// that origin's forms with the agent doing nothing at all.
//
// So the driver arms a window before it runs a click or a key press on something the reading showed,
// and the fence consults it. Three properties, and each one is load-bearing:
//
//   - It lives exactly as long as the ACT does. Armed before the verb, disarmed when Act returns.
//     There is no timeout to tune and no number to get wrong, and "this act caused it" is literally
//     what the arrangement expresses. A form that submits after a 300ms debounce still lands inside
//     it, because the act already waits for the page to react (see afterAct).
//   - It is CONSUMED. One submission per act. A page that submits a second form on the same click
//     finds the window shut.
//   - It carries the ORIGIN the act was on, so the fence can refuse a form aimed somewhere else
//     without knowing anything about pages.
//
// # Why the origin comes from the page and not from the session's url
//
// The document an element lives in is not always the document the session is on: a form in a
// cross-site iframe is a separate origin in a separate process. Asking the page for its own
// location.href, in the same question that finds the form, is the only spelling that is right for
// both — and the answer goes through fence.WriteOriginOf, the same function the fence will use on
// the request, so the two sides cannot disagree about what an origin is.

// writesRemembered bounds what one session accumulates before an act drains it.
//
// The same bound the refusal ring has and for the same reason, though it is far harder to reach:
// one write per act, and an act drains them. A session that accumulated more than this has something
// wrong with it that a longer list would not fix.
const writesRemembered = 64

// fieldsRemembered caps the NAMES kept from one form. browser.Write.FieldCount still reports the
// true total — see the field, and why it is a separate number rather than a length.
const fieldsRemembered = 32

// writeWindow is the permission one act opens: a single form submission, from this origin, for as
// long as the act lasts.
type writeWindow struct {
	// origin is the page's own, in fence.WriteOriginOf's shape.
	origin string
	// form is what the arming question learned, waiting for the two things only the request knows.
	form browser.Write
	// used is what makes it one submission and not a licence. Set under the driver's lock by
	// takeWrite, which is its only writer.
	used bool
}

// formQuestion asks an element which form it would submit, and what that form carries.
//
// A raw string, so there is no backtick anywhere inside it.
//
// # Which elements count
//
// `this.form` for anything that is a form control — a submit button, an input, a select — which also
// covers the case the `form` attribute makes possible, a control belonging to a form it is not
// inside. Then `closest('form')` for everything else, which is the broader half and is deliberate: a
// great many real submit controls are a div with a handler, and a rule that only knew about <button>
// would refuse most of the forms in the product while looking correct.
//
// The known limitation on the other side, and it is named rather than hidden: a control OUTSIDE
// every form that submits one by script arms nothing, and its submission is refused. That is the
// honest cost of tying the window to the element the agent acted on, and the agent is told about it
// — the refusal says the page submitted this itself.
//
// # Names, never values
//
// It reads `name` and never `value`. See browser.Write for why that line is where it is. Unnamed
// controls are skipped because a control with no name is not submitted at all, so its absence here
// is its absence from the request. Names are deduplicated — a radio group is one field asked once,
// not four — and the count counts distinct names for the same reason.
//
// The 256 is a bound on the ANSWER and not on the record: it stops a pathological form turning one
// question into a megabyte of JSON. What the record keeps is fieldsRemembered, applied on the Go
// side, so the two numbers are two different jobs and neither can drift into the other.
const formQuestion = `function() {
  const form = this.form
    || (this.tagName === 'FORM' ? this : (this.closest ? this.closest('form') : null));
  if (!form) { return ''; }
  const seen = [];
  for (const one of Array.from(form.elements || [])) {
    const name = one.name;
    if (!name || one.disabled) { continue; }
    const said = String(name).slice(0, 64);
    if (seen.indexOf(said) === -1) { seen.push(said); }
  }
  return JSON.stringify({
    href: location.href,
    fields: seen.slice(0, 256),
    count: seen.length
  });
}`

// formSeen is the arming question's answer.
type formSeen struct {
	Href   string   `json:"href"`
	Fields []string `json:"fields"`
	Count  int      `json:"count"`
}

// armWrite opens the window for one act, when the element it names is part of a form.
//
// It never fails the act. A page that cannot answer the question, an element in no form, a document
// whose location has no origin the write list could be written in — every one of them leaves the
// window shut, which means a submission that follows is refused and the agent is told why. That is
// the right direction for all of them: the alternative to a refusal here would be a write nobody
// asked for.
//
// It costs one CDP round trip on every click and every press that names something, including the
// great majority that touch no form at all. Stated rather than left to be discovered, and paid
// deliberately: folding the question into the click's own aim question would save it on clicks and
// buy nothing on presses, at the price of one function answering two unrelated questions.
func (d *Driver) armWrite(ctx context.Context, entry *session, on cdp.SessionID, objectID string, action browser.Action) {
	if objectID == "" {
		// A refless press sends its key wherever focus happens to be, and the page can move focus
		// between the reading and the key — so arming on it would mean the agent submitting a form
		// other than the one it believes it is on. The cost is smaller than it looks: a press WITH a
		// ref focuses the element first and then sends the key, so Enter in a search box or a chat
		// is still how a search or a message is sent.
		return
	}
	answer, err := d.callOnValue(ctx, on, objectID, formQuestion, "")
	if err != nil || answer == "" {
		return
	}
	var seen formSeen
	if err := json.Unmarshal([]byte(answer), &seen); err != nil {
		return
	}
	origin := fence.WriteOriginOf(seen.Href)
	if origin == "" {
		return
	}
	names := seen.Fields
	if len(names) > fieldsRemembered {
		names = names[:fieldsRemembered]
	}
	window := &writeWindow{
		origin: origin,
		form: browser.Write{
			Fields:     names,
			FieldCount: seen.Count,
			// Read HERE, at the moment the form is armed, and not when the request is answered. By
			// then the fence has bytes and no page: a multipart body says a file went, and only the
			// document doing the submitting knows what it was called.
			Files: d.attachedNames(ctx, on, objectID),
			Ref:   action.Ref,
			Verb:  string(action.Kind),
		},
	}
	d.mu.Lock()
	entry.mayWrite = window
	d.mu.Unlock()
}

// disarmWrite shuts the window. Called on every path out of an act, armed or not.
func (d *Driver) disarmWrite(entry *session) {
	d.mu.Lock()
	entry.mayWrite = nil
	d.mu.Unlock()
}

// armedWrite finds the window a paused request belongs to, without consuming it.
//
// # Two ways of finding it, and why the second is not the first one guessed
//
// By FRAME first, which is exact: a session's main frame id is fixed when the session is created and
// never reassigned, so it names one session and only one. If a session matched, its answer is final
// whether or not it has a window open — falling through to the second rule there would credit
// another session's act for a request that provably belongs to this one.
//
// A form inside an IFRAME produces a request on the subframe's id, which is no session's main frame,
// so nothing matches and the second rule applies: the one session with a window armed for this exact
// origin. It is the same imprecision recordRefusal already carries, bounded far more tightly — the
// window is open only while an act is in flight, the origin still has to match, and the grant still
// has to exist — so the worst case is two simultaneous acts on the same granted origin crediting
// each other's submission. What it buys is that a form in a frame works at all, which is most login
// forms and a good deal of everything else.
//
// Ambiguity refuses. Two sessions armed for the same origin at the same instant is a state with no
// right answer, and inventing one would file a submission in the wrong session's record.
func (d *Driver) armedWrite(frame, origin string) (*session, *writeWindow) {
	d.mu.Lock()
	defer d.mu.Unlock()

	if frame != "" {
		for _, entry := range d.sessions {
			if entry.frameID != frame {
				continue
			}
			if entry.mayWrite != nil && !entry.mayWrite.used {
				return entry, entry.mayWrite
			}
			return nil, nil
		}
	}

	var found *session
	for _, entry := range d.sessions {
		if entry.mayWrite == nil || entry.mayWrite.used || entry.mayWrite.origin != origin {
			continue
		}
		if found != nil {
			return nil, nil
		}
		found = entry
	}
	if found == nil {
		return nil, nil
	}
	return found, found.mayWrite
}

// takeWrite consumes the window and writes down what left.
//
// Called only after the fence has ALLOWED the request, and that is the whole distinction between
// this record and the refusal record: what is written here is what an agent actually did as the
// person. A submission that was stopped is a refusal and not a write, and conflating the two would
// make the record of what happened include things that did not.
func (d *Driver) takeWrite(entry *session, window *writeWindow, method, url string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if entry == nil || window == nil || window.used {
		return
	}
	window.used = true

	record := window.form
	record.Origin = fence.WriteOriginOf(url)
	record.Method = strings.ToUpper(strings.TrimSpace(method))
	record.Action = whereItWent(url)

	entry.writes = append(entry.writes, record)
	if len(entry.writes) > writesRemembered {
		entry.writes = entry.writes[len(entry.writes)-writesRemembered:]
	}
}

// drainWrites hands an act everything this session has sent and has not yet reported.
//
// Drained rather than accumulated, so a write is reported once. One recorded after this act finished
// reaches the next act by simply still being here, which is the "this act or the next" contract the
// refusal record already has — except that here the race barely exists: a submission produces a
// document, and the act is already waiting for that very document to arrive.
func (d *Driver) drainWrites(entry *session) []browser.Write {
	d.mu.Lock()
	defer d.mu.Unlock()
	if len(entry.writes) == 0 {
		return nil
	}
	drained := entry.writes
	entry.writes = nil
	return drained
}

// whereItWent is the submitted url with the query taken off.
//
// Dropped and not shortened. A form's action can carry a token in it — the pattern is ordinary, and
// `?token=…` is a value like any other — so keeping it would put a credential in the núcleo's
// database through the one field nobody would think to look at. What is left is the origin and the
// path, which is what ties a row to something a person can find on the page.
func whereItWent(raw string) string {
	if cut := strings.IndexAny(raw, "?#"); cut >= 0 {
		return raw[:cut]
	}
	return raw
}
