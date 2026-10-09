package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"sort"
	"strconv"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// Snapshot reads the accessibility tree and mints a stable ref per interesting node.
//
// # Why the accessibility tree and not the DOM
//
// The agent needs to know what is on the page and what it is called. The DOM answers that in tens of
// thousands of tokens of markup, most of it layout; the accessibility tree answers it in a few
// hundred, in the vocabulary a person would use — "button, Sign in". It is also what the page means
// rather than how it is drawn, so a class rename does not invalidate everything the agent knows.
//
// # Why refs and not selectors
//
// A ref can only name something a snapshot actually showed. A CSS selector can be synthesised by the
// agent for an element it never saw — including one a page's text talked it into. Refs make "act on
// something that was not in the snapshot" unrepresentable rather than merely discouraged.
func (d *Driver) Snapshot(ctx context.Context, id browser.SessionID, req browser.SnapshotRequest) (browser.Snapshot, error) {
	d.gate.RLock()
	defer d.gate.RUnlock()
	entry, err := d.lookup(id)
	if err != nil {
		return browser.Snapshot{}, err
	}
	if d.personHolds(id) {
		return browser.Snapshot{}, browser.ErrPersonIsDriving
	}

	root, err := d.readTree(ctx, entry)
	if err != nil {
		return browser.Snapshot{}, err
	}

	// The panel is the person's. Its host and everything under it leave the reading before the walk,
	// so no ref is ever minted for a panel node. Visible sessions only.
	d.withoutPanel(ctx, entry, root)

	// Before the walk, because a link's address is shortened against the page's own origin and the
	// walk is where the elements are built.
	facts := d.locate(ctx, entry.cdp)
	final, title := d.notePlace(entry, facts.URL, facts.Title)
	// An empty readyState is a page that could not be asked, and is not evidence of anything. Said
	// only when something actually says it: a reading that guesses "unfinished" would send the agent
	// round a loop it can never leave.
	_, carrying := d.ferryState(entry)
	stillLoading := (facts.Ready != "" && facts.Ready != "complete") || carrying > 0

	// The cross-origin half. pageQuestion walked every frame it was allowed to touch; these are the
	// ones it was not, and an embedded dashboard is usually one of them.
	facts.Unread = mergeUnread(facts.Unread, d.unreadInFrames(ctx, entry))

	read := collect(root, req, final)
	elements, gone := d.name(entry, read.elements, req.ChangesOnly)

	return browser.Snapshot{
		SessionID:    id,
		URL:          final,
		Title:        title,
		Elements:     elements,
		Truncated:    read.truncated,
		TextNext:     read.textNext,
		ControlsNext: read.controlsNext,
		Gone:         gone,
		// A filtered reading is a partial one for the same reason a differential one is: without
		// this the agent reads a search that found two things as a page with two things on it.
		Partial:      req.ChangesOnly || strings.TrimSpace(req.Find) != "",
		Blocked:      d.blockedSoFar(entry),
		StillLoading: stillLoading,
		Unread:       facts.Unread,
		Dialogs:      d.dialogsSoFar(entry),
		Status:       d.statusOf(entry),
	}, nil
}

// name gives each control the ref it had last time, or a new one, and remembers what it reported.
//
// This is where "e5" becomes a promise. Everything else about a snapshot is a fresh reading of the
// page; this is the one part that is allowed to remember, because the agent remembers too — it holds
// refs from the previous snapshot and acts on them. Minting by position instead, which is what this
// did until 2026-08-19, meant a click that inserted one row silently renamed everything below it.
func (d *Driver) name(entry *session, collected []found, changesOnly bool) ([]browser.Element, []string) {
	d.mu.Lock()
	defer d.mu.Unlock()

	previous := entry.lastReported
	elements := make([]browser.Element, 0, len(collected))
	refs := make(map[string]nodeKey, len(collected))
	reported := make(map[string]browser.Element, len(collected))

	for _, one := range collected {
		element := one.element
		if !one.control {
			// Prose has no ref, so nothing can be said about whether THIS paragraph is the one from
			// last time. On a changes-only read it is dropped rather than guessed at: an agent asked
			// for what moved and repeating the article would be the opposite of the answer.
			if !changesOnly {
				elements = append(elements, element)
			}
			continue
		}

		ref, known := entry.refByNode[one.key]
		if !known {
			entry.mintedRefs++
			ref = fmt.Sprintf("e%d", entry.mintedRefs)
			entry.refByNode[one.key] = ref
		}
		element.Ref = ref
		refs[ref] = one.key
		reported[ref] = element

		// Every control on the page is above this line and only the carried ones are below it. That
		// is what makes `refs` a picture of the PAGE rather than of the last reading of it — a
		// second page of controls used to replace the first, so following `controls_next` quietly
		// invalidated every ref the agent was still holding — and it is what makes `gone` mean "not
		// on the page" instead of "not in this slice of it".
		if !one.carried {
			continue
		}

		if changesOnly {
			if before, seen := previous[ref]; seen && sameElement(before, element) {
				continue
			}
		}
		elements = append(elements, element)
	}

	// What left the page. This is the half omission cannot express: a full snapshot says an element
	// is gone by not containing it, and a partial one says nothing at all by not containing it.
	var gone []string
	if changesOnly {
		for ref := range previous {
			if _, still := reported[ref]; !still {
				gone = append(gone, ref)
			}
		}
		sort.Strings(gone)
	}

	entry.refs = refs
	entry.lastReported = reported
	return elements, gone
}

// sameElement is equality as the AGENT would see it: everything that is reported, and nothing that
// is not. Two elements that differ only in something a snapshot never carries are the same element
// as far as "what changed" can mean.
func sameElement(a, b browser.Element) bool {
	if a.Role != b.Role || a.Name != b.Name || a.Value != b.Value || len(a.State) != len(b.State) {
		return false
	}
	for i := range a.State {
		if a.State[i] != b.State[i] {
			return false
		}
	}
	return true
}

// textBudget bounds how much prose one snapshot carries, in characters.
//
// A bound is not optional: a documentation site or a long thread would otherwise put a megabyte into
// a context window, and the failure mode is not a slow snapshot but a turn that has no room left to
// think in. Roughly five thousand tokens, which is a page a person would call long. Controls are
// NEVER dropped for it — they are what the agent acts on, and a page whose buttons went missing to
// make room for prose would be one the agent cannot use at all.
const textBudget = 20000

// controlBudget bounds how many actionable elements one snapshot carries.
//
// A count and not a character measure, because what an agent pays per control is roughly fixed —
// a ref, a role, a name, sometimes a value and a state — and three hundred of them costs about what
// twenty thousand characters of prose costs. The two budgets are separate so that a long article
// keeps its buttons, which was the rule this design started from and is still right; what was
// missing was the other end, where a directory listing has two thousand links and NOTHING bounded
// them. That snapshot came back saying `truncated: false`, because the prose had fit.
const controlBudget = 300

// slice is what one snapshot managed to carry, and where a next one would resume.
type slice struct {
	elements     []found
	truncated    bool
	textNext     int
	controlsNext int
}

// tableRoles are the parts of a table this walk knows by name.
//
// None of them earns a ref: there is no verb that does anything to a cell. What they earn is their
// SHAPE — a row is emitted as one line with its cells separated, rather than as a stream of loose
// text with the grid dissolved out of it. A column of numbers whose headers are somewhere further
// up, in no stated relation, is a table the agent can read and cannot use.
func isCell(role string) bool {
	switch role {
	case "cell", "gridcell", "columnheader", "rowheader":
		return true
	default:
		return false
	}
}

// rowLine renders one row as its cells, in order, separated.
//
// A cell that holds a control contributes the control's NAME, and the control is still emitted
// separately with its ref. That is the one place this file says something twice on purpose: the
// alternative is a row reading " | 3 days ago | 1.2 kB", with a hole where the link was, and a hole
// in a table is worse than a word repeated. Both halves are bounded, so the cost is capped.
func rowLine(byID map[string]axNode, row axNode) string {
	cells := make([]string, 0, len(row.ChildIDs))
	for _, childID := range row.ChildIDs {
		child, ok := byID[childID]
		if !ok || !isCell(child.Role.Value) {
			continue
		}
		cells = append(cells, cellText(byID, childID, 0))
	}
	if len(cells) == 0 {
		return ""
	}
	if strings.TrimSpace(strings.Join(cells, "")) == "" {
		return ""
	}
	return strings.Join(cells, " | ")
}

// cellText is everything one cell says, flattened.
func cellText(byID map[string]axNode, id string, depth int) string {
	// Bounded rather than trusted, for the same reason the walk guards against cycles: this is a
	// tree Chromium built from somebody else's page.
	if depth > 8 {
		return ""
	}
	node, ok := byID[id]
	if !ok || node.Ignored {
		return ""
	}
	name := strings.TrimSpace(node.Name.Value)
	if interesting(node.Role.Value, name) {
		if name == "" {
			// An unnamed box contributes what is IN it, which is the only thing it has to say.
			// Without this the row reads "Cabo HDMI |  | x" — the hole this function's comment says
			// it exists to avoid, reintroduced by the very controls that were just let through.
			return strings.TrimSpace(node.Value.Value)
		}
		// A control's text IS its name, and descending would say it again.
		return name
	}
	if node.Role.Value == "StaticText" {
		return name
	}
	parts := make([]string, 0, len(node.ChildIDs))
	for _, child := range node.ChildIDs {
		if piece := cellText(byID, child, depth+1); piece != "" {
			parts = append(parts, piece)
		}
	}
	return strings.Join(parts, " ")
}

// found is one line of a snapshot together with the node it came from. Refs are NOT minted here:
// they belong to the session, because a ref has to mean the same element on the next snapshot too,
// and this function sees one page at one instant.
type found struct {
	element browser.Element
	key     nodeKey
	control bool
	// carried says this line is in the answer. A control that a budget, a cursor or a search left
	// out is still collected, because refs are minted from this list and a ref has to keep meaning
	// the same element: dropping it here would make an act on something an EARLIER snapshot showed
	// come back as a stale ref, which is a true sentence about the wrong thing — the element is on
	// the page, and only this reading of it was filtered.
	carried bool
}

// collect walks the tree in document order and decides what earns a line.
//
// Document order is the point, and it is why this does its own depth-first walk over `childIds`
// rather than iterating the array Chromium sent. MEASURED: that array is not reading order — a page
// whose markup runs heading, paragraph, label, checkbox came back as heading, checkbox, checkbox,
// button, button, paragraph, label. Prose is only worth carrying if the agent can tell which control
// it belongs to, and a paragraph filed after the button it describes has lost exactly that.
//
// It takes a tree and not a node list because a page is not always one document: a cross-site frame
// is a separate target with a separate tree, and it is spliced in at the element that holds it for
// the same reason the walk exists at all.
//
// textFrom resumes prose that a previous snapshot could not fit, and the returned offset is where
// this one stopped. Truncation is terminal for prose rather than skip-and-continue: a reading that
// dropped one long paragraph and then included a short one from further down would be a page nobody
// wrote, and the agent has no way to tell that from the page.
func collect(root *tree, req browser.SnapshotRequest, page string) slice {
	elements := make([]found, 0, len(root.nodes))
	// Two pairs and not one. spent/passed are the prose: what this slice delivered, and where the
	// prose has got to overall. controlsSpent/controlsPassed are the same for the actionable set,
	// counted rather than measured. They are bounded apart so a long article keeps its buttons.
	spent, passed := 0, 0
	controlsSpent, controlsPassed := 0, 0
	textCut, controlsCut := false, false

	// says is the Find filter, and an absent one matches everything — which is every snapshot this
	// file took before there was a filter at all.
	//
	// Substring and case-insensitive, deliberately: an agent searching a page is searching for words
	// it read in that page or in the task, and a regular expression here would be a language to get
	// wrong for a gain nobody asked for.
	needle := strings.ToLower(strings.TrimSpace(req.Find))
	says := func(parts ...string) bool {
		if needle == "" {
			return true
		}
		for _, part := range parts {
			if strings.Contains(strings.ToLower(part), needle) {
				return true
			}
		}
		return false
	}

	// A paragraph and a table row are the same thing as far as the budget is concerned: both are
	// what the page SAYS, and neither is something an act can name.
	//
	// The cut is terminal. Skip-and-continue would drop a long paragraph and let a shorter one from
	// further down through, which composes a page nobody wrote and reads exactly like the page.
	say := func(role, text string) {
		switch {
		case text == "" || textCut:
		case !says(text):
			// Filtered out, and not charged for: a search that spent the budget on what it discarded
			// would answer a narrow question at the price of the whole page.
		case passed < req.TextFrom:
			passed += len(text)
		case spent+len(text) > textBudget:
			textCut = true
		default:
			spent += len(text)
			passed += len(text)
			elements = append(elements, found{element: browser.Element{Role: role, Name: text}})
		}
	}

	// One pass per document. Node ids are per document — two documents both call their root "1" —
	// so the maps below are rebuilt for each rather than shared, and a framed document is walked by
	// recursing into this function at the element that holds it.
	var document func(t *tree)
	document = func(t *tree) {
		byID := make(map[string]axNode, len(t.nodes))
		hasParent := make(map[string]bool, len(t.nodes))
		for _, node := range t.nodes {
			byID[node.NodeID] = node
			for _, child := range node.ChildIDs {
				hasParent[child] = true
			}
		}

		// Which nodes are controls, and what they are called. Both are needed to drop text twice
		// over: a control's accessible name comes from its own subtree on a button, and from a
		// SIBLING on `<label>Email <input></label>` — where the label's text is not inside the input
		// at all. An ancestor check alone misses that one, and the agent then sees "Email" as a
		// paragraph and as the box's name, and cannot tell which of the two it should act on.
		control := make(map[string]bool, len(t.nodes))
		named := make(map[string]bool, len(t.nodes))
		for _, node := range t.nodes {
			name := strings.TrimSpace(node.Name.Value)
			if !node.Ignored && interesting(node.Role.Value, name) {
				control[node.NodeID] = true
				named[name] = true
			}
		}

		seen := make(map[string]bool, len(t.nodes))

		var walk func(id string, insideControl, insideRow bool)
		walk = func(id string, insideControl, insideRow bool) {
			// Cycles are not supposed to happen in a tree. This is a tree Chromium built from a page
			// somebody else wrote, so it is guarded rather than trusted.
			if seen[id] {
				return
			}
			seen[id] = true
			node, ok := byID[id]
			if !ok {
				return
			}

			isControl := control[id]
			isRow := node.Role.Value == "row"
			if !node.Ignored {
				switch {
				case isControl:
					name := strings.TrimSpace(node.Name.Value)
					value := strings.TrimSpace(node.Value.Value)
					one := found{
						element: browser.Element{
							Role:  node.Role.Value,
							Name:  name,
							Value: value,
							State: stateOf(node),
							URL:   shortURL(propertyOf(node, "url"), page),
						},
						key:     nodeKey{session: t.session, backend: node.BackendDOMNodeID},
						control: true,
					}
					switch {
					case !says(node.Role.Value, name, value):
						// Not what was asked for. Collected anyway — see found.carried.
					case controlsCut:
						// Past the cut nothing more, in the order the page has it.
					case controlsPassed < req.ControlsFrom:
						controlsPassed++
					case controlsSpent >= controlBudget:
						controlsCut = true
					default:
						controlsSpent++
						controlsPassed++
						one.carried = true
					}
					elements = append(elements, one)
				case isRow:
					// The row as one line, and the text inside it suppressed below. Emitted here so
					// it lands where the row is, which is the whole of what a table is.
					say("row", rowLine(byID, node))
				case node.Role.Value == "StaticText" && !insideControl && !insideRow:
					name := strings.TrimSpace(node.Name.Value)
					// Dropped when some control is already called this. It costs the odd line of
					// prose that happens to repeat a label, and it buys the agent never seeing the
					// same words twice in two roles.
					if !named[name] {
						say("text", name)
					}
				}
			}

			// A document Chromium put in another process hangs off the element that holds it, and is
			// walked HERE so that it lands where it appears rather than after everything.
			if node.BackendDOMNodeID != 0 {
				if inner, framed := t.inner[node.BackendDOMNodeID]; framed {
					document(inner)
				}
			}

			for _, child := range node.ChildIDs {
				walk(child, insideControl || isControl, insideRow || isRow)
			}
		}

		// From the roots, in the order Chromium listed them. A well-formed tree has one; a page
		// mid-load can present several, and starting from each is what keeps the whole page rather
		// than the first fragment of it.
		for _, node := range t.nodes {
			if !hasParent[node.NodeID] {
				walk(node.NodeID, false, false)
			}
		}
		// Anything the walk never reached, because a detached subtree is still on the page. Same
		// order as before, appended rather than interleaved: their position is genuinely unknown.
		for _, node := range t.nodes {
			walk(node.NodeID, false, false)
		}
		// Framed documents whose holder was never found. Same rule, same reason.
		for _, orphan := range t.orphans {
			document(orphan)
		}
	}

	document(root)

	read := slice{elements: elements, truncated: textCut || controlsCut}
	if textCut {
		read.textNext = passed
	}
	if controlsCut {
		read.controlsNext = controlsPassed
	}
	return read
}

// stateOf reports the accessibility properties that change what an act would mean.
//
// Only those. `focused`, `readonly`, `level` and the rest of the AX vocabulary are real and are left
// out: every one costs context, and none of them changes whether the agent should press the thing.
func stateOf(node axNode) []string {
	var state []string
	for _, property := range node.Properties {
		value := strings.TrimSpace(property.Value.Value)
		switch property.Name {
		case "checked", "pressed":
			switch value {
			case "true":
				state = append(state, "checked")
			case "false":
				state = append(state, "unchecked")
			case "mixed":
				state = append(state, "mixed")
			}
		case "expanded":
			if value == "true" {
				state = append(state, "expanded")
			} else if value == "false" {
				state = append(state, "collapsed")
			}
		case "disabled", "selected", "required", "focused":
			if value == "true" {
				state = append(state, property.Name)
			}
		}
	}
	return state
}

// propertyOf reads one accessibility property by name, or "".
func propertyOf(node axNode, name string) string {
	for _, property := range node.Properties {
		if property.Name == name {
			return strings.TrimSpace(property.Value.Value)
		}
	}
	return ""
}

// shortURL is a link's address as the agent should read it.
//
// A path when it points at the page's own origin, which is most links on most pages: it is shorter,
// it is more legible, and `goto` resolves a relative url against the page anyway, so nothing is lost
// by handing back the half that differs. A link to anywhere else keeps its whole address, because
// where it leaves to is the part worth knowing.
//
// The saving is not cosmetic. A listing carries up to a control budget of links, and a full absolute
// url on each of them is comparable to the entire prose budget.
func shortURL(raw, page string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return ""
	}
	target, err := url.Parse(raw)
	if err != nil || !target.IsAbs() {
		return raw
	}
	base, baseErr := url.Parse(page)
	if baseErr != nil || base.Host == "" {
		return raw
	}
	if !strings.EqualFold(target.Scheme, base.Scheme) || !strings.EqualFold(target.Host, base.Host) {
		return raw
	}
	short := target.EscapedPath()
	if short == "" {
		short = "/"
	}
	if target.RawQuery != "" {
		short += "?" + target.RawQuery
	}
	if target.Fragment != "" {
		short += "#" + target.EscapedFragment()
	}
	return short
}

// axValue is one field of an accessibility node, and it is tolerant on purpose.
//
// MEASURED against real Chromium: `properties[].value.value` arrives as a JSON **boolean** for
// `disabled` and as a string for `checked`. A struct that assumed string failed the whole snapshot
// on any page with a disabled control — the first gate run against this caught it, which is what
// that run is for. Numbers are accepted too, because `level` and `setsize` are numbers and the next
// property somebody reads for a good reason will be one of them.
type axValue struct {
	Value string
}

func (v *axValue) UnmarshalJSON(raw []byte) error {
	// The WHOLE object arrives here, not the inner field. Defining UnmarshalJSON takes over decoding
	// of the struct, so the `json:"value"` tag that used to do this no longer runs — an earlier
	// version of this method forgot that, decoded `{"type":"role","value":"button"}` as an
	// unrecognised shape, and put the entire JSON object into every role and name on the page. The
	// snapshot came back with zero elements and no error, which is the quietest way this could have
	// broken.
	var envelope struct {
		Value json.RawMessage `json:"value"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return err
	}
	if len(envelope.Value) == 0 {
		v.Value = ""
		return nil
	}

	var decoded any
	if err := json.Unmarshal(envelope.Value, &decoded); err != nil {
		return err
	}
	switch typed := decoded.(type) {
	case nil:
		v.Value = ""
	case string:
		v.Value = typed
	case bool:
		v.Value = strconv.FormatBool(typed)
	case float64:
		v.Value = strconv.FormatFloat(typed, 'f', -1, 64)
	default:
		// A shape nobody has seen. Rendering it back as JSON keeps the snapshot working and leaves
		// something legible rather than an empty string, which would read as "absent".
		v.Value = string(envelope.Value)
	}
	return nil
}

type axProperty struct {
	Name  string  `json:"name"`
	Value axValue `json:"value"`
}

type axNode struct {
	NodeID           string       `json:"nodeId"`
	Ignored          bool         `json:"ignored"`
	Role             axValue      `json:"role"`
	Name             axValue      `json:"name"`
	Value            axValue      `json:"value"`
	Properties       []axProperty `json:"properties"`
	ChildIDs         []string     `json:"childIds"`
	BackendDOMNodeID int64        `json:"backendDOMNodeId"`
}

// interesting decides what earns a ref.
//
// A snapshot is an input to a context window, so everything included costs attention that something
// else then cannot have. Nodes with no accessible name are dropped even when their role is
// actionable: an unnamed button is one the agent could not describe a reason for pressing, and
// offering it invites a guess.
//
// # The exception, which the rule above got wrong for one whole class of control
//
// That reasoning is about NAMING, and it was applied to everything as though it were about
// ACTIONABILITY. For a button the two coincide: with no name there is nothing to say about what
// pressing it does, so offering it really would be inviting a guess. For a box that HOLDS something
// they come apart completely.
//
// The quantity field in a table row is the case that showed it. `<td>Cabo HDMI</td><td><input
// value=1></td>` — the input has no label, so it had no name, so it got no ref, so there was no way
// to type in it at all. Nothing was ambiguous about it: the row says what it is. What was missing
// was a handle, and dropping it silently meant the agent could not even report that a field existed
// and could not be reached.
//
// `browser_look` sharpened this from a limitation into a contradiction. The picture shows the box,
// drawn among the words that explain it, and the reading has no name for the thing the picture
// shows. Seeing a control you cannot address is worse than not seeing it.
//
// So: a control that holds a value earns a ref whatever it is called, because the page's own text
// around it is what says what it is; a control that only acts still needs a name, because there its
// name is the only thing that could. `cellText` below carries the other half — an unnamed box
// contributes its VALUE to the row, so the row does not read with a hole where the field is.
func interesting(role, name string) bool {
	if name == "" {
		return holdsAValue(role)
	}
	switch role {
	case "button", "link", "textbox", "searchbox", "checkbox", "radio", "combobox",
		"listbox", "menuitem", "tab", "switch", "slider", "heading":
		return true
	default:
		return false
	}
}

// holdsAValue says which controls are worth a ref even with nothing to call them.
//
// Every one of these has a state the agent may need to READ or SET, and every one of them appears
// unlabelled in ordinary applications: the quantity in a table row, the checkbox that selects it,
// the search box whose only label is a placeholder. A `link`, a `menuitem` and a `tab` are
// deliberately absent — they go somewhere, and where they go is what a name would have told you.
func holdsAValue(role string) bool {
	switch role {
	case "textbox", "searchbox", "combobox", "listbox", "checkbox", "radio", "switch",
		"slider", "spinbutton":
		return true
	default:
		return false
	}
}

// pageFacts is everything one Runtime.evaluate can answer about a page at once.
//
// One call, because a snapshot already pays for it. Where the page is, what it is called, whether it
// has finished arriving, and what it is showing that the accessibility tree cannot express — each of
// those separately would be a round trip, and a reading that costs four round trips is one an agent
// stops taking between actions.
type pageFacts struct {
	URL    string           `json:"url"`
	Title  string           `json:"title"`
	Ready  string           `json:"ready"`
	Unread []browser.Unread `json:"unread"`
}

// pageQuestion is what gets evaluated. A raw string with no backtick anywhere inside it, which is a
// property of the file and not of the JavaScript.
//
// The sizes are thresholds and not truth: a one-pixel canvas is a tracking pixel and a six-hundred
// pixel one is a chart, and the point of the whole field is to be worth reading rather than to be
// exhaustive.
//
// It walks SAME-ORIGIN frames as well as the main document, which the first version did not, and the
// gap was not a corner: querySelectorAll does not cross into an iframe's document, so an embedded
// dashboard — the single most likely place for a chart to be — was invisible to a field whose entire
// purpose is to notice charts. A cross-origin frame throws on the first property touched, is caught
// here, and is asked separately over its own target (unreadInFrames): the two halves together are
// what "what is on this page" means.
//
// Bounded on both axes. Depth, because frames nest; breadth, because a page can carry hundreds of
// them and this runs on every snapshot.
const pageQuestion = `JSON.stringify((() => {
  const seen = {};
  const add = (kind, n) => { if (n > 0) { seen[kind] = (seen[kind] || 0) + n; } };
  const big = (el, min) => {
    const box = el.getBoundingClientRect();
    return box.width >= min && box.height >= min;
  };
  const described = (el) =>
    (el.getAttribute('aria-label') || '').trim() !== '' ||
    (el.getAttribute('alt') || '').trim() !== '' ||
    el.querySelector('title, desc') !== null;
  const count = (doc) => {
    add('canvas', [...doc.querySelectorAll('canvas')].filter(el => big(el, 64)).length);
    add('video', [...doc.querySelectorAll('video')].filter(el => big(el, 64)).length);
    add('drawing', [...doc.querySelectorAll('svg')].filter(el => big(el, 64) && !described(el)).length);
    add('image', [...doc.querySelectorAll('img')].filter(el => big(el, 256) && !described(el)).length);
  };
  const walk = (win, depth) => {
    try { count(win.document); } catch (e) { return; }
    if (depth <= 0) { return; }
    const many = Math.min(win.frames.length, 16);
    for (let i = 0; i < many; i++) {
      try { walk(win.frames[i], depth - 1); } catch (e) {}
    }
  };
  try { walk(window, 4); } catch (e) {}
  return {
    url: location.href,
    title: document.title,
    ready: document.readyState,
    unread: Object.keys(seen).map((kind) => ({kind: kind, count: seen[kind]})),
  };
})())`

// framesAsked bounds how many cross-origin frames a reading interrogates.
//
// Each one is a round trip, on every snapshot, and the whole argument for pageFacts is that a
// reading which costs several of those is one an agent stops taking between actions. Four covers the
// embedded-dashboard case this exists for; a page with forty cross-origin frames is an advertising
// page, and counting the fortieth iframe's image would cost the agent more than it tells it.
const framesAsked = 4

// unreadInFrames asks each cross-origin frame what it is showing, and adds it to the page's own.
//
// The same-origin walk inside pageQuestion cannot reach these: touching a cross-origin frame's
// document throws, by the rule the whole browser is built on. They are separate targets with
// separate execution contexts, so the only way to ask is to ask them, one at a time, over their own
// session — which is exactly what the accessibility walk already does for their contents.
//
// Sorted, so that two readings of an unchanged page say the same thing in the same order. Map order
// in Go is deliberately random, and a snapshot that reshuffles its own fields between calls makes a
// changes-only reading report changes that did not happen.
func (d *Driver) unreadInFrames(ctx context.Context, entry *session) []browser.Unread {
	d.mu.Lock()
	sessions := make([]string, 0, len(entry.frames))
	for on := range entry.frames {
		sessions = append(sessions, string(on))
	}
	d.mu.Unlock()
	sort.Strings(sessions)

	var found []browser.Unread
	for i, on := range sessions {
		if i >= framesAsked {
			break
		}
		found = mergeUnread(found, d.locate(ctx, cdp.SessionID(on)).Unread)
	}
	return found
}

// mergeUnread adds one frame's tally to the running one, keeping first-seen order.
//
// By KIND and not by frame, because the agent is not going to act on any of it. "Two canvases" is
// the whole of what it needs: that the page shows something this reading does not carry, and roughly
// how much of it. Which document each one sits in would be detail with nothing on the other end.
func mergeUnread(into, more []browser.Unread) []browser.Unread {
	for _, one := range more {
		found := false
		for i := range into {
			if into[i].Kind == one.Kind {
				into[i].Count += one.Count
				found = true
				break
			}
		}
		if !found {
			into = append(into, one)
		}
	}
	return into
}

// locate asks the page the one question a snapshot needs answered about it.
func (d *Driver) locate(ctx context.Context, cdpSession cdp.SessionID) pageFacts {
	result, err := d.conn.Call(ctx, cdpSession, "Runtime.evaluate", map[string]any{
		"expression":    pageQuestion,
		"returnByValue": true,
	})
	if err != nil {
		return pageFacts{}
	}
	var payload struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return pageFacts{}
	}
	var facts pageFacts
	if err := json.Unmarshal([]byte(payload.Result.Value), &facts); err != nil {
		return pageFacts{}
	}
	return facts
}

// panelHost is the node name of the panel's host element, as the DOM reports it.
const panelHost = "NUCLEOS-PANEL"

// withoutPanel drops the panel host's node and its whole subtree from the main document's reading.
// A session without a panel is left alone and no call is made. Best effort: when the host cannot be
// found the reading is unchanged, which is what a page without a panel yet looks like.
func (d *Driver) withoutPanel(ctx context.Context, entry *session, root *tree) {
	d.mu.Lock()
	visible := d.visible
	d.mu.Unlock()
	if !visible || entry.panel == nil || root == nil {
		return
	}
	// Depth 2: the html element's children are what holds the host, and depth 1 stops short of them.
	raw, err := d.conn.Call(ctx, entry.cdp, "DOM.getDocument", map[string]any{"depth": 2})
	if err != nil {
		return
	}
	var doc struct {
		Root dnode `json:"root"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		return
	}
	hosts := map[int64]bool{}
	var find func(n dnode, depth int)
	find = func(n dnode, depth int) {
		if depth > 3 {
			return
		}
		if strings.EqualFold(n.NodeName, panelHost) && n.BackendNodeID != 0 {
			hosts[n.BackendNodeID] = true
		}
		for _, child := range n.Children {
			find(child, depth+1)
		}
	}
	find(doc.Root, 0)
	if len(hosts) == 0 {
		return
	}

	byID := make(map[string]axNode, len(root.nodes))
	for _, node := range root.nodes {
		byID[node.NodeID] = node
	}
	drop := map[string]bool{}
	var mark func(id string)
	mark = func(id string) {
		if drop[id] {
			return
		}
		drop[id] = true
		for _, child := range byID[id].ChildIDs {
			mark(child)
		}
	}
	for _, node := range root.nodes {
		if hosts[node.BackendDOMNodeID] {
			mark(node.NodeID)
		}
	}
	if len(drop) == 0 {
		return
	}
	kept := make([]axNode, 0, len(root.nodes))
	for _, node := range root.nodes {
		if drop[node.NodeID] {
			continue
		}
		var children []string
		for _, child := range node.ChildIDs {
			if !drop[child] {
				children = append(children, child)
			}
		}
		node.ChildIDs = children
		kept = append(kept, node)
	}
	root.nodes = kept
}

// dnode is the part of a DOM node that finding the panel host reads.
type dnode struct {
	NodeName      string  `json:"nodeName"`
	BackendNodeID int64   `json:"backendNodeId"`
	Children      []dnode `json:"children"`
}
