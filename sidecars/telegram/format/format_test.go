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

// Telegram parses every chunk on its own, so a tag cut in half is not a tag, it is a parse error —
// and the plain-text fallback for that error cannot strip `<a hre` either, because the regex needs
// the closing `>`. Both halves of that failure start with the cut landing mid-tag.
func TestChunkNeverCutsInsideATagOrAnEntity(t *testing.T) {
	const limit = 32
	cases := map[string]string{
		"tag straddles the limit":    strings.Repeat("a", 28) + `<a href="https://x.test/">link</a>` + strings.Repeat("b", 40),
		"entity straddles the limit": strings.Repeat("a", 30) + "&quot;" + strings.Repeat("b", 40),
	}

	for name, source := range cases {
		t.Run(name, func(t *testing.T) {
			chunks := Chunk(source, limit)
			if strings.Join(chunks, "") != source {
				t.Fatalf("Chunk() lost or reordered content: %q", chunks)
			}
			for i, chunk := range chunks {
				if utf8.RuneCountInString(chunk) > limit {
					t.Errorf("chunk %d length = %d, want <= %d", i, utf8.RuneCountInString(chunk), limit)
				}
				if open := strings.LastIndex(chunk, "<"); open >= 0 && !strings.Contains(chunk[open:], ">") {
					t.Errorf("chunk %d ends inside a tag: %q", i, chunk)
				}
				if amp := strings.LastIndex(chunk, "&"); amp >= 0 && !strings.Contains(chunk[amp:], ";") {
					t.Errorf("chunk %d ends inside an entity: %q", i, chunk)
				}
			}
		})
	}
}

// StripTags is the plain-text fallback. Handed a tag whose `>` never arrived it used to leave the
// fragment in place, so the message a user reads when everything else failed was garbled markup.
func TestStripTagsRemovesATagThatWasCutOff(t *testing.T) {
	if got := StripTags(`hello <a hre`); got != "hello " {
		t.Errorf("StripTags() = %q, want the truncated tag gone", got)
	}
}

// The italic pattern used to run before the link pattern, so an underscore inside a URL became
// markup and the link pattern no longer recognised what was left.
func TestUnderscoresInsideALinkURLAreNotItalics(t *testing.T) {
	got := ToHTML("[docs](https://ex.com/a_b_c)")
	if !strings.Contains(got, `href="https://ex.com/a_b_c"`) {
		t.Errorf("ToHTML() = %q, want the URL kept intact", got)
	}
	if strings.Contains(got, "<i>") {
		t.Errorf("ToHTML() = %q, want no italics inside the URL", got)
	}
}

// inline() holds code spans and links aside behind NUL-delimited placeholders. Orchestrator output
// carrying that byte sequence used to be restored as if it were one of them — one span rendered
// twice, and a message that could choose what it turned into.
func TestInlinePlaceholdersCannotBeForgedByTheInput(t *testing.T) {
	got := ToHTML("\x000\x00 and `real`")
	if strings.Count(got, "<code>") != 1 {
		t.Errorf("ToHTML() = %q, want exactly one code span (the real one)", got)
	}
	if strings.ContainsRune(got, 0) {
		t.Errorf("ToHTML() = %q, want no NUL bytes left in the output", got)
	}
}
