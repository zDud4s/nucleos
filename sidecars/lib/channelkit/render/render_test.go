package render

import (
	"errors"
	"strings"
	"testing"
	"unicode/utf8"
)

// Every length in this file is counted in runes, the same unit Render uses for maxText.

// upperRenderer is a fake native renderer: it "converts" by upper-casing and chunks with the
// plain chunker.
type upperRenderer struct{ PlainRenderer }

func (upperRenderer) ToNative(md string) (string, error) { return strings.ToUpper(md), nil }

// refusingRenderer always reports the markdown as unrenderable.
type refusingRenderer struct{ PlainRenderer }

func (refusingRenderer) ToNative(string) (string, error) { return "", ErrUnrenderable }

// brokenRenderer fails with an error that is not ErrUnrenderable.
type brokenRenderer struct{ PlainRenderer }

var errBoom = errors.New("boom")

func (brokenRenderer) ToNative(string) (string, error) { return "", errBoom }

func join(ps []Piece) string {
	var b strings.Builder
	for _, p := range ps {
		b.WriteString(p.Text)
	}
	return b.String()
}

func TestRenderNoPieceExceedsMaxText(t *testing.T) {
	md := strings.Repeat("line of text\n", 50) + strings.Repeat("x", 200)
	for _, max := range []int{1, 2, 7, 13, 50, 4096} {
		for _, r := range []Renderer{PlainRenderer{}, upperRenderer{}, refusingRenderer{}} {
			ps, err := Render(r, md, max)
			if err != nil {
				t.Fatalf("max %d: %v", max, err)
			}
			for i, p := range ps {
				if n := utf8.RuneCountInString(p.Text); n > max {
					t.Fatalf("max %d: piece %d has %d runes", max, i, n)
				}
				if p.Text == "" {
					t.Fatalf("max %d: piece %d is empty", max, i)
				}
			}
		}
	}
}

func TestRenderNeverSplitsUTF8(t *testing.T) {
	md := strings.Repeat("ação 日本語 😀 ñ\n", 30)
	for max := 1; max <= 20; max++ {
		ps, err := Render(PlainRenderer{}, md, max)
		if err != nil {
			t.Fatal(err)
		}
		for i, p := range ps {
			if !utf8.ValidString(p.Text) {
				t.Fatalf("max %d: piece %d is not valid UTF-8: %q", max, i, p.Text)
			}
		}
		if join(ps) != md {
			t.Fatalf("max %d: pieces do not reassemble the input", max)
		}
	}
}

func TestRenderEmptyTextYieldsNoPieces(t *testing.T) {
	for _, r := range []Renderer{PlainRenderer{}, upperRenderer{}, refusingRenderer{}} {
		ps, err := Render(r, "", 10)
		if err != nil || len(ps) != 0 {
			t.Fatalf("got %v, %v", ps, err)
		}
	}
}

func TestRenderUnrenderableFallsBackToPlain(t *testing.T) {
	md := "alpha\nbeta\ngamma\ndelta"
	ps, err := Render(refusingRenderer{}, md, 8)
	if err != nil {
		t.Fatal(err)
	}
	if len(ps) < 2 {
		t.Fatalf("expected several pieces, got %d", len(ps))
	}
	for i, p := range ps {
		if p.Native {
			t.Fatalf("piece %d is marked native", i)
		}
	}
	if join(ps) != md {
		t.Fatalf("got %q", join(ps))
	}
}

func TestRenderNativeMarksPieces(t *testing.T) {
	ps, err := Render(upperRenderer{}, "abc\ndef", 4)
	if err != nil {
		t.Fatal(err)
	}
	if len(ps) == 0 || join(ps) != "ABC\nDEF" {
		t.Fatalf("got %v", ps)
	}
	for i, p := range ps {
		if !p.Native {
			t.Fatalf("piece %d should be native", i)
		}
	}
}

func TestRenderOtherErrorIsReturned(t *testing.T) {
	ps, err := Render(brokenRenderer{}, "text", 10)
	if !errors.Is(err, errBoom) || ps != nil {
		t.Fatalf("got %v, %v", ps, err)
	}
}

func TestRenderPreservesOrder(t *testing.T) {
	var b strings.Builder
	for i := 0; i < 40; i++ {
		b.WriteString("n")
		b.WriteString(strings.Repeat("0", i%5))
		b.WriteString("\n")
	}
	md := b.String()
	ps, err := Render(PlainRenderer{}, md, 9)
	if err != nil {
		t.Fatal(err)
	}
	if join(ps) != md {
		t.Fatal("order or content changed")
	}
}

func TestRenderLimitOne(t *testing.T) {
	md := "añ😀\nz"
	ps, err := Render(PlainRenderer{}, md, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(ps) != utf8.RuneCountInString(md) {
		t.Fatalf("got %d pieces", len(ps))
	}
	if join(ps) != md {
		t.Fatalf("got %q", join(ps))
	}
}

func TestPlainPrefersLineBoundary(t *testing.T) {
	got := PlainRenderer{}.Chunk("aaa\nbbb\nccc", 8)
	want := []string{"aaa\nbbb\n", "ccc"}
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("got %q", got)
	}
}

func TestPlainRendererIsIdentity(t *testing.T) {
	r := PlainRenderer{}
	n, err := r.ToNative("**x**")
	if err != nil || n != "**x**" || r.Plain("**x**") != "**x**" {
		t.Fatalf("got %q, %v", n, err)
	}
}
