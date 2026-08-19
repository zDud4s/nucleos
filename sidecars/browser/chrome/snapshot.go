package chrome

import (
	"context"
	"encoding/json"
	"fmt"
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
	entry, err := d.lookup(id)
	if err != nil {
		return browser.Snapshot{}, err
	}

	root, err := d.readTree(ctx, entry)
	if err != nil {
		return browser.Snapshot{}, err
	}

	read := collect(root, req)
	elements, gone := d.name(entry, read.elements, req.ChangesOnly)

	url, title := d.locate(ctx, entry.cdp)
	if url != "" {
		entry.final = url
	}
	if title != "" {
		entry.title = title
	}

	return browser.Snapshot{
		SessionID:    id,
		URL:          entry.final,
		Title:        entry.title,
		Elements:     elements,
		Truncated:    read.truncated,
		TextNext:     read.textNext,
		ControlsNext: read.controlsNext,
		Gone:         gone,
		Partial:      req.ChangesOnly,
		Blocked:      d.blockedSoFar(entry),
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
func collect(root *tree, req browser.SnapshotRequest) slice {
	elements := make([]found, 0, len(root.nodes))
	// Two pairs and not one. spent/passed are the prose: what this slice delivered, and where the
	// prose has got to overall. controlsSpent/controlsPassed are the same for the actionable set,
	// counted rather than measured. They are bounded apart so a long article keeps its buttons.
	spent, passed := 0, 0
	controlsSpent, controlsPassed := 0, 0
	textCut, controlsCut := false, false

	// A paragraph and a table row are the same thing as far as the budget is concerned: both are
	// what the page SAYS, and neither is something an act can name.
	//
	// The cut is terminal. Skip-and-continue would drop a long paragraph and let a shorter one from
	// further down through, which composes a page nobody wrote and reads exactly like the page.
	say := func(role, text string) {
		switch {
		case text == "" || textCut:
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
					switch {
					case controlsCut:
						// Past the cut nothing more, in the order the page has it.
					case controlsPassed < req.ControlsFrom:
						controlsPassed++
					case controlsSpent >= controlBudget:
						controlsCut = true
					default:
						controlsSpent++
						controlsPassed++
						elements = append(elements, found{
							element: browser.Element{
								Role:  node.Role.Value,
								Name:  strings.TrimSpace(node.Name.Value),
								Value: strings.TrimSpace(node.Value.Value),
								State: stateOf(node),
							},
							key:     nodeKey{session: t.session, backend: node.BackendDOMNodeID},
							control: true,
						})
					}
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
		case "disabled", "selected", "required":
			if value == "true" {
				state = append(state, property.Name)
			}
		}
	}
	return state
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
func interesting(role, name string) bool {
	if name == "" {
		return false
	}
	switch role {
	case "button", "link", "textbox", "searchbox", "checkbox", "radio", "combobox",
		"listbox", "menuitem", "tab", "switch", "slider", "heading":
		return true
	default:
		return false
	}
}

func (d *Driver) locate(ctx context.Context, cdpSession cdp.SessionID) (url, title string) {
	result, err := d.conn.Call(ctx, cdpSession, "Runtime.evaluate", map[string]any{
		"expression":    "JSON.stringify({url: location.href, title: document.title})",
		"returnByValue": true,
	})
	if err != nil {
		return "", ""
	}
	var payload struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return "", ""
	}
	var located struct {
		URL   string `json:"url"`
		Title string `json:"title"`
	}
	if err := json.Unmarshal([]byte(payload.Result.Value), &located); err != nil {
		return "", ""
	}
	return located.URL, located.Title
}
