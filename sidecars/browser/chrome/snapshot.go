package chrome

import (
	"context"
	"encoding/json"
	"fmt"
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
func (d *Driver) Snapshot(ctx context.Context, id browser.SessionID) (browser.Snapshot, error) {
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

	elements := make([]browser.Element, 0, len(payload.Nodes))
	refs := make(map[string]int64, len(payload.Nodes))
	for _, node := range payload.Nodes {
		if node.Ignored {
			continue
		}
		role := node.Role.Value
		name := strings.TrimSpace(node.Name.Value)
		if !interesting(role, name) {
			continue
		}
		ref := fmt.Sprintf("e%d", len(elements)+1)
		refs[ref] = node.BackendDOMNodeID
		elements = append(elements, browser.Element{Ref: ref, Role: role, Name: name})
	}

	d.mu.Lock()
	entry.refs = refs
	d.mu.Unlock()

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
	}, nil
}

type axValue struct {
	Value string `json:"value"`
}

type axNode struct {
	NodeID           string  `json:"nodeId"`
	Ignored          bool    `json:"ignored"`
	Role             axValue `json:"role"`
	Name             axValue `json:"name"`
	BackendDOMNodeID int64   `json:"backendDOMNodeId"`
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
