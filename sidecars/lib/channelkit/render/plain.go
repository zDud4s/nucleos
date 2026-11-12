package render

import "unicode/utf8"

// PlainRenderer is the default Renderer: it performs no markup conversion and cuts text into
// pieces of at most limit runes, preferring line boundaries and never splitting a UTF-8 sequence.
type PlainRenderer struct{}

// ToNative returns md unchanged.
func (PlainRenderer) ToNative(md string) (string, error) { return md, nil }

// Plain returns md unchanged.
func (PlainRenderer) Plain(md string) string { return md }

// Chunk implements Renderer for plain text; see ChunkPlain.
func (PlainRenderer) Chunk(native string, limit int) []string { return ChunkPlain(native, limit) }

// ChunkPlain splits s into non-empty pieces of at most limit runes (limit below 1 counts as 1).
// Within the window it cuts after the last newline when there is one, otherwise at the window end.
// The pieces concatenate back to s. Empty input yields no pieces.
func ChunkPlain(s string, limit int) []string {
	if limit < 1 {
		limit = 1
	}
	var out []string
	for s != "" {
		if utf8.RuneCountInString(s) <= limit {
			out = append(out, s)
			break
		}
		// end is the byte offset just past the limit-th rune; nl is the offset just past the last
		// newline inside the window (0 when there is none).
		end, nl, runes := 0, 0, 0
		for end < len(s) && runes < limit {
			r, size := utf8.DecodeRuneInString(s[end:])
			end += size
			runes++
			if r == '\n' {
				nl = end
			}
		}
		cut := end
		if nl > 0 {
			cut = nl
		}
		out = append(out, s[:cut])
		s = s[cut:]
	}
	return out
}
