package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"

	"nucleosbrowser/cdp"
)

// This file is about one measured fact: the accessibility tree stops at a process boundary.
//
// `Accessibility.getFullAXTree` is asked of a TARGET. A same-process iframe is part of the page's
// tree and needs nothing; a cross-site one is a separate process with a separate target, and the
// page's tree contains the <iframe> element and nothing inside it. The gate measured this on
// 2026-08-19 — a page framing a form on another host snapshotted as an empty list — and it is the
// worst shape of failure this pillar has: the reading is CORRECT for the target it was taken of, so
// nothing reports a problem, and the agent concludes the form does not exist. It is also the case
// the pillar exists for, since an SSO form is usually framed.
//
// The driver already knew about the boundary for the fence: Connect re-arms auto-attach on every
// target because a cross-site frame is otherwise never offered at all. This is the same fact applied
// to reading instead of to blocking.

// tree is one document, plus the documents framed inside it.
//
// A tree per document rather than one flat node list, because node ids are per document: two
// documents both call their root "1", and merging them would make a walk follow the wrong children.
type tree struct {
	session cdp.SessionID
	nodes   []axNode
	// inner is keyed by the backend node id of the <iframe> element that holds each child, so the
	// walk can splice a framed document in WHERE IT APPEARS. Position is not cosmetic here: prose
	// only earns its place in a snapshot if the agent can tell which control it belongs to, and a
	// framed form appended after everything has lost exactly that.
	inner map[int64]*tree
	// orphans are framed documents whose holder could not be located in this one. Kept and appended
	// rather than dropped: a login form in the wrong place is still the login form, and losing it is
	// the failure this whole file exists to fix.
	orphans []*tree
}

// framed is one child document as this session knows it.
type framed struct {
	session cdp.SessionID
	target  string
}

// frameDepth bounds the descent. A page can frame a page that frames a page, and the bound is here
// so that a hostile or merely broken one costs a fixed number of round trips rather than all of
// them. Eight is far past anything a real login flow does.
const frameDepth = 8

// readTree reads the page's accessibility tree and every framed document hanging off it.
func (d *Driver) readTree(ctx context.Context, entry *session) (*tree, error) {
	d.mu.Lock()
	children := map[cdp.SessionID][]framed{}
	for child, info := range entry.frames {
		children[info.parent] = append(children[info.parent], framed{session: child, target: info.target})
	}
	d.mu.Unlock()

	// Ordered, because map iteration is not. Two snapshots of a page that did not move must not
	// differ, or every changes-only read would report the frames as having swapped places.
	for parent := range children {
		siblings := children[parent]
		sort.Slice(siblings, func(i, j int) bool { return siblings[i].target < siblings[j].target })
	}

	root, err := d.readDocument(ctx, entry.cdp)
	if err != nil {
		return nil, err
	}
	d.attachFrames(ctx, root, children, 0)
	return root, nil
}

// attachFrames reads each child document and hangs it off the element that holds it.
func (d *Driver) attachFrames(ctx context.Context, parent *tree, children map[cdp.SessionID][]framed, depth int) {
	if depth >= frameDepth {
		return
	}
	for _, child := range children[parent.session] {
		inner, err := d.readDocument(ctx, child.session)
		if err != nil {
			// A frame that navigated or closed between the attach and this read is not a failed
			// snapshot. The rest of the page is still a true reading, and refusing to return it
			// would make a page with one flaky ad frame unreadable.
			continue
		}
		d.attachFrames(ctx, inner, children, depth+1)
		if owner, ok := d.frameOwner(ctx, parent.session, child.target); ok {
			parent.inner[owner] = inner
		} else {
			parent.orphans = append(parent.orphans, inner)
		}
	}
}

// readDocument reads one target's accessibility tree.
func (d *Driver) readDocument(ctx context.Context, session cdp.SessionID) (*tree, error) {
	if _, err := d.conn.Call(ctx, session, "Accessibility.enable", nil); err != nil {
		return nil, fmt.Errorf("enabling accessibility: %w", err)
	}
	raw, err := d.conn.Call(ctx, session, "Accessibility.getFullAXTree", nil)
	if err != nil {
		return nil, fmt.Errorf("reading the accessibility tree: %w", err)
	}
	var payload struct {
		Nodes []axNode `json:"nodes"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, err
	}
	return &tree{session: session, nodes: payload.Nodes, inner: map[int64]*tree{}}, nil
}

// frameOwner finds the <iframe> element in the parent document that holds a framed target.
//
// The target id doubles as the frame id for an out-of-process frame, which is what makes this one
// call rather than a walk of the frame tree. When it does not answer — a renamed method, a frame
// that detached, a nesting this did not follow — the caller keeps the document as an orphan. Losing
// the position is a cost; losing the document is the bug.
func (d *Driver) frameOwner(ctx context.Context, parent cdp.SessionID, frameID string) (int64, bool) {
	if _, err := d.conn.Call(ctx, parent, "DOM.enable", nil); err != nil {
		return 0, false
	}
	result, err := d.conn.Call(ctx, parent, "DOM.getFrameOwner", map[string]any{"frameId": frameID})
	if err != nil {
		return 0, false
	}
	var payload struct {
		BackendNodeID int64 `json:"backendNodeId"`
	}
	if err := json.Unmarshal(result, &payload); err != nil || payload.BackendNodeID == 0 {
		return 0, false
	}
	return payload.BackendNodeID, true
}
