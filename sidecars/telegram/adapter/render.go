package adapter

import (
	"fmt"
	"regexp"
	"strings"

	"nucleoschannelkit/render"
	"nucleostelegram/format"
)

// tagToken matches one HTML tag; group 1 is "/" for a closing tag, group 2 the tag name.
var tagToken = regexp.MustCompile(`<(/?)([a-zA-Z]+)(?:\s[^<>]*)?>`)

// telegramRenderer renders neutral markdown to Telegram HTML via the v1 format package.
type telegramRenderer struct {
	// toHTML is injectable so tests can force markup that cannot be sent.
	toHTML func(string) string
}

// NewRenderer returns the Telegram render.Renderer.
func NewRenderer() render.Renderer {
	return &telegramRenderer{toHTML: format.ToHTML}
}

// ToNative converts md to Telegram HTML, or returns render.ErrUnrenderable when the result has
// unbalanced tags (Telegram would reject it, so the caller falls back to plain text).
func (r *telegramRenderer) ToNative(md string) (string, error) {
	html := r.toHTML(md)
	if err := checkBalanced(html); err != nil {
		return "", fmt.Errorf("%w: %v", render.ErrUnrenderable, err)
	}
	return html, nil
}

// Chunk splits Telegram HTML into pieces of at most limit runes. format.Chunk never cuts inside a
// <pre> block and closes/reopens oversized ones, so each piece stays balanced.
func (r *telegramRenderer) Chunk(native string, limit int) []string {
	return format.Chunk(native, limit)
}

// Plain returns the markdown with markup stripped.
func (r *telegramRenderer) Plain(md string) string {
	return format.StripTags(r.toHTML(md))
}

// checkBalanced reports an error when tags do not nest and close properly, or a tag is left open.
func checkBalanced(html string) error {
	var stack []string
	for _, m := range tagToken.FindAllStringSubmatch(html, -1) {
		name := strings.ToLower(m[2])
		if m[1] == "" {
			stack = append(stack, name)
			continue
		}
		if len(stack) == 0 || stack[len(stack)-1] != name {
			return fmt.Errorf("unexpected </%s>", name)
		}
		stack = stack[:len(stack)-1]
	}
	if len(stack) > 0 {
		return fmt.Errorf("unclosed <%s>", stack[len(stack)-1])
	}
	return nil
}
