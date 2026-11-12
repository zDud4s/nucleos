// §spec pilar-de-web

// Package extract turns a fetched HTML page into the Markdown the núcleo stores and the model reads.
//
// Extraction happens here, in the sidecar, rather than in the núcleo, for the same reason the fetch
// does: this is where somebody else's bytes are, and parsing hostile markup is the part most likely
// to behave badly. What crosses back to the núcleo is text.
//
// It is also why the núcleo never sees HTML. A trust decision (spec §5) is made over a document
// whose scripts, iframes, tracking pixels and hidden elements are already gone — Markdown carries
// no capability to execute anything, so the worst a page can do downstream is be persuasive, which
// is what §6 is for.
package extract

import (
	"errors"
	"fmt"
	"strings"

	readability "github.com/mackee/go-readability"
)

// Status says how much of the page survived extraction. It is stored alongside the text because a
// reader — person or model — deserves to know whether it is looking at an article or at the
// scrapings of one.
type Status string

const (
	// StatusArticle: readability found a main content root and it is what you are reading.
	StatusArticle Status = "article"
	// StatusFallback: no article root scored high enough, so this is the page's accessibility tree
	// flattened. Navigation-heavy pages (an index, a search result, a dashboard) land here
	// legitimately — it is a shape, not a failure.
	StatusFallback Status = "fallback"
)

// ErrEmpty is returned when there is nothing worth storing. Distinct from a fetch failure: the
// server answered, the bytes arrived, and there was no readable content in them. Callers must not
// cache an empty page as though it were a successful read.
var ErrEmpty = errors.New("no readable content")

// MinContentChars is the floor below which a result is treated as empty rather than as a very short
// article. Consent banners and "enable JavaScript" interstitials extract cleanly to about a
// sentence, and storing those as the page would be worse than admitting the read failed — this is
// the single knob that decides whether the browser (spec §3.5) is needed for a given site.
const MinContentChars = 140

// Article is what crosses back to the núcleo.
type Article struct {
	Title    string `json:"title"`
	Byline   string `json:"byline"`
	Markdown string `json:"markdown"`
	Status   Status `json:"status"`
}

// Extract parses one HTML document. It never returns partial content alongside an error: a caller
// that stored both would have to decide which to believe.
func Extract(html string) (Article, error) {
	if strings.TrimSpace(html) == "" {
		return Article{}, ErrEmpty
	}

	// The document is parsed here rather than through readability.Extract, because the fallback
	// below needs it.
	//
	// MEASURED against go-readability v0.3.1: `ReadabilityArticle.AriaTree` is ALWAYS nil. Setting
	// `options.GenerateAriaTree = true` changes nothing — inside `ExtractContent` the branch that
	// would populate it contains only the comment "AriaTree generation would be implemented here".
	// The field and the option are both real; the code between them is not. So the tree is built
	// here from the exported `BuildAriaTree`, which does work. A future version that fills the field
	// in would make this redundant, not wrong.
	document, err := readability.ParseHTML(html, "")
	if err != nil {
		return Article{}, fmt.Errorf("parse: %w", err)
	}
	readability.PreprocessDocument(document)

	parsed := readability.ExtractContent(document, readability.DefaultOptions())

	title := strings.TrimSpace(parsed.Title)

	if parsed.Root != nil {
		markdown := strings.TrimSpace(readability.ToMarkdown(parsed.Root))
		if len([]rune(markdown)) >= MinContentChars {
			return Article{
				Title:    title,
				Byline:   strings.TrimSpace(parsed.Byline),
				Markdown: markdown,
				Status:   StatusArticle,
			}, nil
		}
	}

	// The fallback is deliberately the ARIA tree and not "the body, tags stripped". A stripped body
	// is every menu item and cookie notice in reading order, which is exactly the shape that fills a
	// context window with nothing. The accessibility tree at least keeps the page's structure.
	if tree := readability.BuildAriaTree(document); tree != nil {
		flattened := strings.TrimSpace(readability.AriaTreeToString(tree))
		if len([]rune(flattened)) >= MinContentChars {
			return Article{
				Title:    title,
				Markdown: flattened,
				Status:   StatusFallback,
			}, nil
		}
	}

	return Article{}, ErrEmpty
}
