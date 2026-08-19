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
func (d *Driver) Snapshot(ctx context.Context, id browser.SessionID, changesOnly bool) (browser.Snapshot, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return browser.Snapshot{}, err
	}

	if _, err := d.conn.Call(ctx, entry.cdp, "Accessibility.enable", nil); err != nil {
		return browser.Snapshot{}, fmt.Errorf("enabling accessibility: %w", err)
	}
	raw, err := d.conn.Call(ctx, entry.cdp, "Accessibility.getFullAXTree", nil)
	if err != nil {
		return browser.Snapshot{}, fmt.Errorf("reading the accessibility tree: %w", err)
	}

	var payload struct {
		Nodes []axNode `json:"nodes"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return browser.Snapshot{}, err
	}

	collected, truncated := collect(payload.Nodes)
	elements, gone := d.name(entry, collected, changesOnly)

	url, title := d.locate(ctx, entry.cdp)
	if url != "" {
		entry.final = url
	}
	if title != "" {
		entry.title = title
	}

	return browser.Snapshot{
		SessionID: id,
		URL:       entry.final,
		Title:     entry.title,
		Elements:  elements,
		Truncated: truncated,
		Gone:      gone,
		Partial:   changesOnly,
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
	refs := make(map[string]int64, len(collected))
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

		ref, known := entry.refByNode[one.backend]
		if !known {
			entry.mintedRefs++
			ref = fmt.Sprintf("e%d", entry.mintedRefs)
			entry.refByNode[one.backend] = ref
		}
		element.Ref = ref
		refs[ref] = one.backend
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

// collect walks the tree in document order and decides what earns a line.
//
// Document order is the point, and it is why this does its own depth-first walk over `childIds`
// rather than iterating the array Chromium sent. MEASURED: that array is not reading order — a page
// whose markup runs heading, paragraph, label, checkbox came back as heading, checkbox, checkbox,
// button, button, paragraph, label. Prose is only worth carrying if the agent can tell which control
// it belongs to, and a paragraph filed after the button it describes has lost exactly that.
// found is one line of a snapshot together with the node it came from. Refs are NOT minted here:
// they belong to the session, because a ref has to mean the same element on the next snapshot too,
// and this function sees one page at one instant.
type found struct {
	element browser.Element
	backend int64
	control bool
}

func collect(nodes []axNode) ([]found, bool) {
	byID := make(map[string]axNode, len(nodes))
	hasParent := make(map[string]bool, len(nodes))
	for _, node := range nodes {
		byID[node.NodeID] = node
		for _, child := range node.ChildIDs {
			hasParent[child] = true
		}
	}

	// Which nodes are controls, and what they are called. Both are needed to drop text twice over:
	// a control's accessible name comes from its own subtree on a button, and from a SIBLING on
	// `<label>Email <input></label>` — where the label's text is not inside the input at all. An
	// ancestor check alone misses that one, and the agent then sees "Email" as a paragraph and as
	// the box's name, and cannot tell which of the two it should act on.
	control := make(map[string]bool, len(nodes))
	named := make(map[string]bool, len(nodes))
	for _, node := range nodes {
		name := strings.TrimSpace(node.Name.Value)
		if !node.Ignored && interesting(node.Role.Value, name) {
			control[node.NodeID] = true
			named[name] = true
		}
	}

	elements := make([]found, 0, len(nodes))
	spent, truncated := 0, false
	seen := make(map[string]bool, len(nodes))

	var walk func(id string, insideControl bool)
	walk = func(id string, insideControl bool) {
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
		if !node.Ignored {
			switch {
			case isControl:
				elements = append(elements, found{
					element: browser.Element{
						Role:  node.Role.Value,
						Name:  strings.TrimSpace(node.Name.Value),
						Value: strings.TrimSpace(node.Value.Value),
						State: stateOf(node),
					},
					backend: node.BackendDOMNodeID,
					control: true,
				})
			case node.Role.Value == "StaticText" && !insideControl:
				name := strings.TrimSpace(node.Name.Value)
				// Dropped when some control is already called this. It costs the odd line of prose
				// that happens to repeat a label, and it buys the agent never seeing the same words
				// twice in two roles.
				if name != "" && !named[name] {
					if spent+len(name) > textBudget {
						truncated = true
					} else {
						spent += len(name)
						elements = append(elements, found{
							element: browser.Element{Role: "text", Name: name},
						})
					}
				}
			}
		}

		for _, child := range node.ChildIDs {
			walk(child, insideControl || isControl)
		}
	}

	// From the roots, in the order Chromium listed them. A well-formed tree has one; a page mid-load
	// can present several, and starting from each is what keeps the whole page rather than the first
	// fragment of it.
	for _, node := range nodes {
		if !hasParent[node.NodeID] {
			walk(node.NodeID, false)
		}
	}
	// Anything the walk never reached, because a detached subtree is still on the page. Same order as
	// before, appended rather than interleaved: their position is genuinely unknown.
	for _, node := range nodes {
		walk(node.NodeID, false)
	}
	return elements, truncated
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
