// Package render turns agent markdown into per-channel text: a Renderer converts to the channel's
// native markup and chunks it, and Render falls back to plain text when the markup cannot be
// produced. All lengths are counted in runes.
package render

import "errors"

// ErrUnrenderable is returned by ToNative when the markdown cannot be expressed in the channel's
// native markup; Render then falls back to Plain.
var ErrUnrenderable = errors.New("render: markdown not representable in native markup")

// Renderer is the per-adapter rendering contract.
type Renderer interface {
	// ToNative converts markdown to the channel's native markup.
	ToNative(md string) (native string, err error)
	// Chunk splits native text into pieces of at most limit runes, keeping the markup balanced.
	Chunk(native string, limit int) []string
	// Plain strips markdown down to readable plain text.
	Plain(md string) string
}

// Piece is one outgoing message body. Native is false when it is plain text.
type Piece struct {
	Text   string
	Native bool
}

// Render converts md with r and cuts it into pieces of at most maxText runes, in order. If ToNative
// returns ErrUnrenderable the plain text is chunked instead and every piece has Native=false. Any
// other ToNative error is returned as is. Empty text yields no pieces.
func Render(r Renderer, md string, maxText int) ([]Piece, error) {
	if md == "" {
		return nil, nil
	}
	native, err := r.ToNative(md)
	if err != nil && !errors.Is(err, ErrUnrenderable) {
		return nil, err
	}
	if err == nil {
		return pieces(r.Chunk(native, maxText), true), nil
	}
	return pieces(ChunkPlain(r.Plain(md), maxText), false), nil
}

func pieces(texts []string, native bool) []Piece {
	var out []Piece
	for _, t := range texts {
		if t != "" {
			out = append(out, Piece{Text: t, Native: native})
		}
	}
	return out
}
