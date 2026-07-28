package format

import (
	"strings"
	"testing"
	"unicode/utf8"
)

func TestToHTMLInlineBoldAndCode(t *testing.T) {
	got := ToHTML("**bold** and `code`")
	if !strings.Contains(got, "<b>bold</b>") {
		t.Errorf("ToHTML() = %q, want bold tag", got)
	}
	if !strings.Contains(got, "<code>code</code>") {
		t.Errorf("ToHTML() = %q, want code tag", got)
	}
}

func TestToHTMLEscapesProse(t *testing.T) {
	got := ToHTML("1 < 2")
	if !strings.Contains(got, "&lt;") {
		t.Errorf("ToHTML() = %q, want escaped less-than", got)
	}
	if strings.Contains(got, "<") {
		t.Errorf("ToHTML() = %q, want no raw tag in prose", got)
	}
}

// escapeHTML runs before linkPattern substitutes the URL into href="$2", so a quote inside the URL
// closes the attribute early and the rest of the URL becomes stray markup. The link regex forbids
// whitespace, which is the only reason a second attribute cannot follow -- the escaping, not the
// regex, is what should be holding that line.
func TestALinkURLCannotCloseItsOwnHrefAttribute(t *testing.T) {
	got := ToHTML(`[click](https://a.test/"x)`)
	if strings.Contains(got, `href="https://a.test/"x"`) {
		t.Errorf("ToHTML() = %q, the URL closed its own href attribute", got)
	}
	if !strings.Contains(got, "&quot;") {
		t.Errorf("ToHTML() = %q, want the quote escaped inside the attribute", got)
	}
}

// StripTags is the fallback a user reads when Telegram rejects the HTML send, so it must decode
// every entity escapeHTML can emit -- and decode each one exactly once.
func TestStripTagsDecodesEveryEscapedEntityOnce(t *testing.T) {
	for _, c := range []struct{ in, want string }{
		{escapeHTML(`say "hi" & <bye>`), `say "hi" & <bye>`},
		{"&amp;lt;", "&lt;"},
		{"<b>x</b>", "x"},
	} {
		if got := StripTags(c.in); got != c.want {
			t.Errorf("StripTags(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

func TestToHTMLTable(t *testing.T) {
	md := "| ID | Modo | Raiz |\n|---|---|---|\n| nucleos | off | — |\n| nucleos-e2e | shadow | C:\\path\\to\\project |"
	got := ToHTML(md)
	if !strings.Contains(got, "<b>nucleos</b>") || !strings.Contains(got, "<b>nucleos-e2e</b>") {
		t.Errorf("ToHTML() = %q, want first-column values rendered as bold titles", got)
	}
	if !strings.Contains(got, "Modo: off") || !strings.Contains(got, "Modo: shadow") {
		t.Errorf("ToHTML() = %q, want remaining columns rendered as Header: value", got)
	}
	if !strings.Contains(got, "Raiz: ") || !strings.Contains(got, `C:\path\to\project`) {
		t.Errorf("ToHTML() = %q, want path rendered under Raiz", got)
	}
	if strings.Contains(got, "|---|") {
		t.Errorf("ToHTML() = %q, want separator row removed", got)
	}
	if strings.Contains(got, "<pre>") {
		t.Errorf("ToHTML() = %q, want table rendered without pre block", got)
	}
}

func TestToHTMLFencedCode(t *testing.T) {
	got := ToHTML("```\nfoo()\n```")
	if !strings.Contains(got, "<pre>") || !strings.Contains(got, "foo()") {
		t.Errorf("ToHTML() = %q, want fenced code in pre block", got)
	}
}

func TestChunkHardSplitsLongLine(t *testing.T) {
	s := strings.Repeat("a", 5000)
	chunks := Chunk(s, 4096)
	if len(chunks) != 2 {
		t.Fatalf("Chunk() returned %d chunks, want 2", len(chunks))
	}
	total := 0
	for _, chunk := range chunks {
		length := utf8.RuneCountInString(chunk)
		if length > 4096 {
			t.Errorf("chunk length = %d, want <= 4096", length)
		}
		total += length
	}
	if total != 5000 {
		t.Errorf("concatenated length = %d, want 5000", total)
	}
}

func TestChunkNeverSplitsPreBlock(t *testing.T) {
	s := "x\n<pre>" + strings.Repeat("y\n", 100) + "</pre>\nz"
	chunks := Chunk(s, 64)
	for i, chunk := range chunks {
		if strings.Count(chunk, "<pre>") != strings.Count(chunk, "</pre>") {
			t.Errorf("chunk %d has unbalanced pre tags: %q", i, chunk)
		}
		if utf8.RuneCountInString(chunk) > 64 {
			t.Errorf("chunk %d length = %d, want <= 64", i, utf8.RuneCountInString(chunk))
		}
	}
}

func TestStripTags(t *testing.T) {
	got := StripTags("<b>hi</b> &amp; <code>x</code>")
	if got != "hi & x" {
		t.Errorf("StripTags() = %q, want %q", got, "hi & x")
	}
}
