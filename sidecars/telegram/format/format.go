// Package format converts orchestrator output into Telegram-friendly messages.
package format

import (
	"regexp"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

var (
	headingPattern     = regexp.MustCompile(`^#{1,6}\s+(.*)$`)
	bulletPattern      = regexp.MustCompile(`^\s*[-*+]\s+(.*)$`)
	orderedPattern     = regexp.MustCompile(`^\s*(\d+\.)\s+(.*)$`)
	codePattern        = regexp.MustCompile("`([^`\\n]*)`")
	boldStarPattern    = regexp.MustCompile(`\*\*(.+?)\*\*`)
	boldUnderPattern   = regexp.MustCompile(`__(.+?)__`)
	italicStarPattern  = regexp.MustCompile(`(^|[^*])\*([^*\s](?:[^*]*[^*\s])?)\*`)
	italicUnderPattern = regexp.MustCompile(`_(.+?)_`)
	linkPattern        = regexp.MustCompile(`\[(.+?)\]\((https?://[^)\s]+)\)`)
	tagPattern         = regexp.MustCompile(`(?s)<[^>]*>`)
	// A tag left open at the very end of the text — what a cut mid-tag leaves behind.
	truncatedTagPattern = regexp.MustCompile(`(?s)<[^>]*$`)
)

// maxEntityLength bounds how far back a cut looks for the `&` that opens an entity. `&thetasym;` is
// among the longest named entities; escapeHTML itself never emits more than `&quot;`.
const maxEntityLength = 12

// ToHTML converts the subset of Markdown that claude emits into Telegram-supported HTML
// (for parse_mode=HTML). Telegram supports <b> <i> <u> <s> <code> <pre> <a>; it supports NO
// tables, so a Markdown table is rendered as a vertical list of records.
func ToHTML(md string) string {
	lines := strings.Split(md, "\n")
	output := make([]string, 0, len(lines))

	for i := 0; i < len(lines); {
		line := lines[i]

		if strings.HasPrefix(line, "```") {
			i++
			start := i
			for i < len(lines) && !strings.HasPrefix(lines[i], "```") {
				i++
			}
			output = append(output, "<pre>"+escapeHTML(strings.Join(lines[start:i], "\n"))+"</pre>")
			if i < len(lines) {
				i++
			}
			continue
		}

		if startsTableRow(line) && i+1 < len(lines) && isTableSeparator(lines[i+1]) {
			end := i + 2
			for end < len(lines) && startsTableRow(lines[end]) {
				end++
			}
			output = append(output, renderTable(lines[i:end]))
			i = end
			continue
		}

		if match := headingPattern.FindStringSubmatch(line); match != nil {
			output = append(output, "<b>"+escapeHTML(match[1])+"</b>")
			i++
			continue
		}
		if match := bulletPattern.FindStringSubmatch(line); match != nil {
			output = append(output, "• "+inline(match[1]))
			i++
			continue
		}
		if match := orderedPattern.FindStringSubmatch(line); match != nil {
			output = append(output, match[1]+" "+inline(match[2]))
			i++
			continue
		}

		output = append(output, inline(line))
		i++
	}

	return strings.Join(output, "\n")
}

// Chunk splits s into pieces each no longer than limit runes, breaking preferentially at newline
// boundaries and never inside a <pre>...</pre> block. Oversized pre blocks are closed and reopened
// so every resulting chunk stays both balanced and under the limit.
func Chunk(s string, limit int) []string {
	if s == "" {
		return []string{""}
	}
	if limit <= 0 {
		return []string{s}
	}

	tokens := tokenizePreBlocks(s, limit)
	chunks := make([]string, 0, (utf8.RuneCountInString(s)/limit)+1)
	current := ""
	currentLen := 0

	flush := func() {
		if current == "" {
			return
		}
		chunks = append(chunks, current)
		current = ""
		currentLen = 0
	}

	for _, token := range tokens {
		if token.pre {
			tokenLen := utf8.RuneCountInString(token.text)
			if currentLen > 0 && currentLen+tokenLen > limit {
				flush()
			}
			current += token.text
			currentLen += tokenLen
			if currentLen >= limit {
				flush()
			}
			continue
		}

		remaining := token.text
		for remaining != "" {
			available := limit - currentLen
			if available == 0 {
				flush()
				available = limit
			}

			if utf8.RuneCountInString(remaining) <= available {
				current += remaining
				currentLen += utf8.RuneCountInString(remaining)
				break
			}

			prefix, rest := splitTextAt(remaining, available)
			current += prefix
			currentLen += utf8.RuneCountInString(prefix)
			flush()
			remaining = rest
		}
	}

	flush()
	if len(chunks) == 0 {
		return []string{""}
	}
	return chunks
}

// StripTags removes HTML tags and unescapes the basic entities, for the plain-text fallback when a
// parse_mode=HTML send is rejected by Telegram.
func StripTags(html string) string {
	plain := tagPattern.ReplaceAllString(html, "")
	// A tag whose `>` never arrived is still markup to a reader. tagPattern cannot match it (it
	// needs the closing bracket), so the fallback used to show `<a hre` as text.
	plain = truncatedTagPattern.ReplaceAllString(plain, "")
	// Mirrors escapeHTML entry for entry, `&quot;` included: this is the fallback a user actually
	// reads when Telegram rejects the HTML send, so an entity escapeHTML can emit and this cannot
	// decode surfaces as raw `&quot;` in their chat.
	//
	// One `strings.Replacer` pass, not sequential ReplaceAll calls: sequential ones would decode
	// `&amp;lt;` twice and hand back `<`, re-materialising markup the escape had neutralised.
	return strings.NewReplacer(
		"&lt;", "<",
		"&gt;", ">",
		"&quot;", `"`,
		"&amp;", "&",
	).Replace(plain)
}

func inline(text string) string {
	// Placeholders below are delimited by NUL, a byte no message legitimately carries. Dropping any
	// the input already had is what stops a line from forging one: a forged placeholder used to be
	// restored as if it were a span this function had held itself, rendering someone else's content
	// twice and letting the input pick what it turned into.
	text = strings.ReplaceAll(text, "\x00", "")

	held := make([]string, 0)
	hold := func(html string) string {
		held = append(held, html)
		return "\x00" + strconv.Itoa(len(held)-1) + "\x00"
	}

	// Code spans and links are both held aside BEFORE emphasis runs. Emphasis inside them is not
	// emphasis: an underscore in `.../a_b_c` is part of a URL, and the italic pattern used to
	// rewrite it into markup that the link pattern then no longer matched.
	protected := codePattern.ReplaceAllStringFunc(text, func(span string) string {
		return hold("<code>" + escapeHTML(span[1:len(span)-1]) + "</code>")
	})
	protected = linkPattern.ReplaceAllStringFunc(protected, func(link string) string {
		parts := linkPattern.FindStringSubmatch(link)
		return hold(`<a href="` + escapeHTML(parts[2]) + `">` + emphasize(escapeHTML(parts[1])) + `</a>`)
	})

	converted := emphasize(escapeHTML(protected))

	// Restored last-to-first: a held fragment can only contain placeholders created before it (a
	// code span inside a link label), so descending order is what leaves none behind.
	for i := len(held) - 1; i >= 0; i-- {
		converted = strings.ReplaceAll(converted, "\x00"+strconv.Itoa(i)+"\x00", held[i])
	}
	return converted
}

func emphasize(escaped string) string {
	converted := boldStarPattern.ReplaceAllString(escaped, "<b>$1</b>")
	converted = boldUnderPattern.ReplaceAllString(converted, "<b>$1</b>")
	converted = italicStarPattern.ReplaceAllString(converted, "$1<i>$2</i>")
	return italicUnderPattern.ReplaceAllString(converted, "<i>$1</i>")
}

// escapeHTML escapes for BOTH contexts this package emits: element text and the one attribute value
// it builds, `href="..."` in inline(). The quote is what makes it safe in the second: without it a
// URL carrying `"` closes the attribute early and the remainder becomes stray markup. Today the
// link regex forbids whitespace, so nothing can follow with a new attribute name, but that is the
// regex holding a line that escaping should hold.
//
// `strings.Replacer` scans once and does not re-examine what it wrote, so `<` -> `&lt;` cannot be
// re-escaped into `&amp;lt;` by the `&` rule that precedes it.
func escapeHTML(text string) string {
	return strings.NewReplacer(
		"&", "&amp;",
		"<", "&lt;",
		">", "&gt;",
		`"`, "&quot;",
	).Replace(text)
}

func startsTableRow(line string) bool {
	return strings.HasPrefix(strings.TrimSpace(line), "|")
}

func isTableSeparator(line string) bool {
	trimmed := strings.TrimSpace(line)
	if !strings.HasPrefix(trimmed, "|") || !strings.Contains(trimmed, "-") {
		return false
	}
	for _, r := range trimmed {
		if r != '|' && r != '-' && r != ':' && !unicode.IsSpace(r) {
			return false
		}
	}
	return true
}

func parseTableRow(line string) []string {
	cells := strings.Split(strings.TrimSpace(line), "|")
	if len(cells) > 0 && strings.TrimSpace(cells[0]) == "" {
		cells = cells[1:]
	}
	if len(cells) > 0 && strings.TrimSpace(cells[len(cells)-1]) == "" {
		cells = cells[:len(cells)-1]
	}
	for i := range cells {
		cells[i] = strings.TrimSpace(cells[i])
	}
	return cells
}

func renderTable(lines []string) string {
	header := parseTableRow(lines[0])
	records := make([]string, 0, len(lines)-2)

	for _, line := range lines[2:] {
		row := parseTableRow(line)
		firstCell := ""
		if len(row) > 0 {
			firstCell = row[0]
		}

		record := []string{"<b>" + inline(firstCell) + "</b>"}
		for column := 1; column < len(row); column++ {
			label := "Campo " + strconv.Itoa(column)
			if column < len(header) {
				label = escapeHTML(header[column])
			}
			record = append(record, label+": "+inline(row[column]))
		}
		records = append(records, strings.Join(record, "\n"))
	}

	return strings.Join(records, "\n\n")
}

type chunkToken struct {
	text string
	pre  bool
}

func tokenizePreBlocks(s string, limit int) []chunkToken {
	const openTag = "<pre>"
	const closeTag = "</pre>"
	tokens := make([]chunkToken, 0)

	for cursor := 0; cursor < len(s); {
		openOffset := strings.Index(s[cursor:], openTag)
		if openOffset < 0 {
			tokens = append(tokens, chunkToken{text: s[cursor:]})
			break
		}
		open := cursor + openOffset
		if open > cursor {
			tokens = append(tokens, chunkToken{text: s[cursor:open]})
		}

		contentsStart := open + len(openTag)
		closeOffset := strings.Index(s[contentsStart:], closeTag)
		if closeOffset < 0 {
			tokens = append(tokens, chunkToken{text: s[open:]})
			break
		}
		close := contentsStart + closeOffset
		end := close + len(closeTag)
		block := s[open:end]
		if utf8.RuneCountInString(block) <= limit {
			tokens = append(tokens, chunkToken{text: block, pre: true})
		} else {
			contentLimit := limit - utf8.RuneCountInString(openTag+closeTag)
			if contentLimit <= 0 {
				tokens = append(tokens, chunkToken{text: block})
			} else {
				for _, part := range splitText(s[contentsStart:close], contentLimit) {
					tokens = append(tokens, chunkToken{text: openTag + part + closeTag, pre: true})
				}
			}
		}
		cursor = end
	}

	return tokens
}

func splitText(s string, limit int) []string {
	if s == "" {
		return []string{""}
	}
	parts := make([]string, 0, (utf8.RuneCountInString(s)/limit)+1)
	for s != "" {
		part, rest := splitTextAt(s, limit)
		parts = append(parts, part)
		s = rest
	}
	return parts
}

func splitTextAt(s string, limit int) (string, string) {
	if utf8.RuneCountInString(s) <= limit {
		return s, ""
	}

	cut := len(s)
	lastNewline := -1
	runes := 0
	for byteIndex, r := range s {
		if runes == limit {
			cut = byteIndex
			break
		}
		runes++
		if r == '\n' {
			lastNewline = byteIndex + 1
		}
	}
	if lastNewline > 0 {
		cut = lastNewline
	} else {
		cut = safeCut(s, cut)
	}
	return s[:cut], s[cut:]
}

// safeCut moves a hard cut off the middle of a tag or an entity. Telegram parses each chunk on its
// own, so `<a hre` in one message and the rest in the next is not a link, it is a rejected message
// — and the plain-text fallback for that rejection cannot strip half a tag either. A newline cut
// never needs this: neither a tag nor an entity contains one.
func safeCut(s string, cut int) int {
	if open := strings.LastIndexByte(s[:cut], '<'); open > 0 && !strings.Contains(s[open:cut], ">") {
		return open
	}
	if amp := strings.LastIndexByte(s[:cut], '&'); amp > 0 && cut-amp <= maxEntityLength &&
		!strings.ContainsAny(s[amp:cut], ";<> ") {
		return amp
	}
	// Falling through leaves the raw cut, which is what keeps a chunk whose first byte opens a tag
	// from producing an empty prefix and a loop that never advances.
	return cut
}
