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
)

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
	code := make([]string, 0)
	protected := codePattern.ReplaceAllStringFunc(text, func(span string) string {
		code = append(code, span[1:len(span)-1])
		return "\x00" + strconv.Itoa(len(code)-1) + "\x00"
	})

	converted := escapeHTML(protected)
	converted = boldStarPattern.ReplaceAllString(converted, "<b>$1</b>")
	converted = boldUnderPattern.ReplaceAllString(converted, "<b>$1</b>")
	converted = italicStarPattern.ReplaceAllString(converted, "$1<i>$2</i>")
	converted = italicUnderPattern.ReplaceAllString(converted, "<i>$1</i>")
	converted = linkPattern.ReplaceAllString(converted, `<a href="$2">$1</a>`)

	for i, contents := range code {
		placeholder := "\x00" + strconv.Itoa(i) + "\x00"
		converted = strings.ReplaceAll(converted, placeholder, "<code>"+escapeHTML(contents)+"</code>")
	}
	return converted
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
	}
	return s[:cut], s[cut:]
}
