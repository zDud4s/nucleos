package contract

import (
	"encoding/json"
	"fmt"
	"strconv"
)

// Limits of annex C.2 (T8) on the capabilities of an adapter.
const (
	minMaxText = 1
	maxMaxText = 1_000_000
)

// MediaKind is a kind of media an adapter can receive or send.
type MediaKind string

const (
	MediaAudio MediaKind = "audio"
	MediaImage MediaKind = "image"
	MediaFile  MediaKind = "file"
)

// UnmarshalJSON accepts only the known kinds; anything else is a decode error.
func (k *MediaKind) UnmarshalJSON(b []byte) error {
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	switch MediaKind(s) {
	case MediaAudio, MediaImage, MediaFile:
		*k = MediaKind(s)
		return nil
	}
	return fmt.Errorf("unknown media kind %q", s)
}

// Capabilities is what an adapter declares in its hello. Optional fields at
// their default value are omitted when encoding (T6); MaxText is required.
type Capabilities struct {
	Buttons  bool        `json:"buttons,omitempty"`
	Edit     bool        `json:"edit,omitempty"`
	Typing   bool        `json:"typing,omitempty"`
	Threads  bool        `json:"threads,omitempty"`
	ReplyTo  bool        `json:"reply_to,omitempty"`
	MaxText  *int        `json:"max_text" kit:"required"`
	MediaIn  []MediaKind `json:"media_in,omitempty"`
	MediaOut []MediaKind `json:"media_out,omitempty"`
}

// Hello is the adapter's presentation (an input shape: decode it strictly).
type Hello struct {
	Kind         string        `json:"kind"`
	Version      string        `json:"version"`
	Capabilities *Capabilities `json:"capabilities" kit:"required"`
}

// Validate checks the grammars, the max_text range and media-kind uniqueness.
func (h *Hello) Validate() *Error {
	if !kindRe.MatchString(h.Kind) {
		return errGrammar("/kind")
	}
	if _, ok := parseSemver(h.Version); !ok {
		return errGrammar("/version")
	}
	c := h.Capabilities
	if *c.MaxText < minMaxText || *c.MaxText > maxMaxText {
		return errRange("/capabilities/max_text")
	}
	if err := uniqueKinds(c.MediaIn, "/capabilities/media_in"); err != nil {
		return err
	}
	return uniqueKinds(c.MediaOut, "/capabilities/media_out")
}

func uniqueKinds(kinds []MediaKind, at string) *Error {
	seen := map[MediaKind]bool{}
	for i, k := range kinds {
		if seen[k] {
			return errUnique(at + "/" + strconv.Itoa(i))
		}
		seen[k] = true
	}
	return nil
}

// HelloResponse is the core's 200 answer to a hello (an output shape).
type HelloResponse struct {
	ContractVersion *string `json:"contract_version" kit:"required"`
}

// Validate checks that the version is MAJOR.MINOR.PATCH.
func (r *HelloResponse) Validate() *Error {
	if _, ok := parseSemver(*r.ContractVersion); !ok {
		return errGrammar("/contract_version")
	}
	return nil
}
