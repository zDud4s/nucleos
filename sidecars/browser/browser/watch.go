// §spec browser-ao-vivo

package browser

import "context"

// Frame is one picture of a page, JPEG-encoded. Meta says how the picture maps back onto the page;
// it is nil when the driver has no geometry to give. A Frame with Prompt set is a question for the
// viewer and carries no picture.
type Frame struct {
	JPEG   []byte
	Meta   *FrameMeta
	Prompt *Prompt
}

// FrameMeta is the geometry of one frame: the picture's own size in pixels and the page's, which a
// viewer needs to turn a click on the picture into a point on the page.
type FrameMeta struct {
	FrameWidth      int     `json:"frameWidth"`
	FrameHeight     int     `json:"frameHeight"`
	DeviceWidth     float64 `json:"deviceWidth"`
	DeviceHeight    float64 `json:"deviceHeight"`
	OffsetTop       float64 `json:"offsetTop"`
	PageScaleFactor float64 `json:"pageScaleFactor"`
	ScrollOffsetX   float64 `json:"scrollOffsetX"`
	ScrollOffsetY   float64 `json:"scrollOffsetY"`
}

// EndReason says why a watch ended.
type EndReason string

const (
	EndClosed EndReason = "closed"
	EndWheel  EndReason = "wheel"
	EndGone   EndReason = "gone"
)

// WatchEnded is the error a watch ends with when the session ended under it, rather than the viewer
// leaving.
type WatchEnded struct{ Reason EndReason }

func (e WatchEnded) Error() string { return "browser: watch ended: " + string(e.Reason) }

// Watcher is the optional ability to stream a session's page to a viewer.
//
// It sits outside Driver on purpose: watching is for a person, not an agent verb, so it must not
// grow the surface the agent is given. Watch blocks until ctx ends or the session ends. The sink
// may block; the implementation must never let a blocked sink stall the browser.
type Watcher interface {
	Watch(ctx context.Context, id SessionID, sink func(Frame)) error
}
