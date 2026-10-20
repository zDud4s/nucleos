package adapter

import (
	"encoding/json"
	"errors"
	"io/fs"
	"strings"
	"testing"
	"unicode/utf8"

	"nucleoschannelkit/contract/fixtures"
	"nucleoschannelkit/render"
)

// sendFullMarkdown loads the markdown sample from the shared outbox/send-full fixture.
func sendFullMarkdown(t *testing.T) string {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "outbox/send-full.json")
	if err != nil {
		t.Fatal(err)
	}
	var msg struct {
		Parts []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"parts"`
	}
	if err := json.Unmarshal(b, &msg); err != nil {
		t.Fatal(err)
	}
	for _, p := range msg.Parts {
		if p.Type == "text" {
			return p.Text
		}
	}
	t.Fatal("no text part in send-full fixture")
	return ""
}

func assertBalanced(t *testing.T, piece string) {
	t.Helper()
	for _, tag := range []string{"b", "i", "u", "s", "code", "pre", "a"} {
		open := strings.Count(piece, "<"+tag+">") + strings.Count(piece, "<"+tag+" ")
		closed := strings.Count(piece, "</"+tag+">")
		if open != closed {
			t.Fatalf("unbalanced <%s> (%d open, %d close) in piece %.80q", tag, open, closed, piece)
		}
	}
}

func TestRenderSendFullMarkdown(t *testing.T) {
	md := sendFullMarkdown(t) + "\n\n```\nfn main() {}\n```"
	pieces, err := render.Render(NewRenderer(), md, 4096)
	if err != nil {
		t.Fatal(err)
	}
	if len(pieces) == 0 {
		t.Fatal("no pieces")
	}
	var all strings.Builder
	for _, p := range pieces {
		if !p.Native {
			t.Fatalf("piece is not native: %q", p.Text)
		}
		if n := utf8.RuneCountInString(p.Text); n > 4096 {
			t.Fatalf("piece has %d runes", n)
		}
		all.WriteString(p.Text)
	}
	for _, want := range []string{"<b>", "<code>", "<pre>", "<a href="} {
		if !strings.Contains(all.String(), want) {
			t.Fatalf("rendered output lacks %s:\n%s", want, all.String())
		}
	}
}

func TestRenderLongCodeBlockStaysBalancedPerPiece(t *testing.T) {
	md := "intro **bold**\n\n```\n" + strings.Repeat("x", 5000) + "\n```\n\nafter `code` [l](https://example.org)"
	pieces, err := render.Render(NewRenderer(), md, 4096)
	if err != nil {
		t.Fatal(err)
	}
	if len(pieces) < 2 {
		t.Fatalf("got %d pieces, want at least 2", len(pieces))
	}
	for _, p := range pieces {
		if n := utf8.RuneCountInString(p.Text); n > 4096 {
			t.Fatalf("piece has %d runes", n)
		}
		assertBalanced(t, p.Text)
	}
}

func TestRenderLongTextSplitsIntoManyPieces(t *testing.T) {
	line := strings.Repeat("word ", 19) + "end\n"
	md := strings.Repeat(line, 100) // about 10000 characters
	pieces, err := render.Render(NewRenderer(), md, 4096)
	if err != nil {
		t.Fatal(err)
	}
	if len(pieces) < 3 {
		t.Fatalf("got %d pieces, want at least 3", len(pieces))
	}
	for _, p := range pieces {
		if utf8.RuneCountInString(p.Text) > 4096 {
			t.Fatalf("piece too long")
		}
	}
}

func TestRenderUnbalancedHTMLFallsBackToPlain(t *testing.T) {
	r := &telegramRenderer{toHTML: func(string) string { return "<b>sem fecho" }}
	if _, err := r.ToNative("**sem fecho"); !errors.Is(err, render.ErrUnrenderable) {
		t.Fatalf("ToNative err = %v, want ErrUnrenderable", err)
	}
	pieces, err := render.Render(r, "**sem fecho", 4096)
	if err != nil {
		t.Fatal(err)
	}
	if len(pieces) != 1 || pieces[0].Native {
		t.Fatalf("pieces = %+v, want one plain piece", pieces)
	}
	if strings.Contains(pieces[0].Text, "<") {
		t.Fatalf("plain text still has markup: %q", pieces[0].Text)
	}
}
