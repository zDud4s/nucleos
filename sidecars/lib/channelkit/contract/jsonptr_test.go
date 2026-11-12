package contract_test

import (
	"strconv"
	"strings"
	"testing"
)

// resolvePointer follows an RFC 6901 pointer over an interpreted JSON value (maps,
// slices, scalars). "" is the document itself. The bool is false when the pointer
// does not lead anywhere.
func resolvePointer(doc any, ptr string) (any, bool) {
	if ptr == "" {
		return doc, true
	}
	if !strings.HasPrefix(ptr, "/") {
		return nil, false
	}
	cur := doc
	for _, raw := range strings.Split(ptr[1:], "/") {
		tok := strings.ReplaceAll(strings.ReplaceAll(raw, "~1", "/"), "~0", "~")
		switch v := cur.(type) {
		case map[string]any:
			next, ok := v[tok]
			if !ok {
				return nil, false
			}
			cur = next
		case []any:
			if tok == "" || (len(tok) > 1 && tok[0] == '0') {
				return nil, false
			}
			i, err := strconv.Atoi(tok)
			if err != nil || i < 0 || i >= len(v) {
				return nil, false
			}
			cur = v[i]
		default:
			return nil, false
		}
	}
	return cur, true
}

func TestResolvePointer(t *testing.T) {
	doc := map[string]any{
		"a/b": []any{"x", map[string]any{"m~n": 7}},
		"":    "empty-key",
	}
	cases := []struct {
		ptr  string
		want any
		ok   bool
	}{
		{"", doc, true},
		{"/", "empty-key", true},
		{"/a~1b/0", "x", true},
		{"/a~1b/1/m~0n", 7, true},
		{"/a~1b/2", nil, false},
		{"/a~1b/01", nil, false},
		{"/a~1b/-", nil, false},
		{"/missing", nil, false},
		{"no-slash", nil, false},
	}
	for _, c := range cases {
		got, ok := resolvePointer(doc, c.ptr)
		if ok != c.ok {
			t.Errorf("%q: ok = %v, want %v", c.ptr, ok, c.ok)
			continue
		}
		if ok && c.ptr != "" {
			if got != c.want {
				t.Errorf("%q: got %v, want %v", c.ptr, got, c.want)
			}
		}
	}
}
