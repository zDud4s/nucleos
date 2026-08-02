package extract

import (
	"errors"
	"strings"
	"testing"
)

// readability scores candidates by text length, so a fixture has to be a real article's worth of
// prose or the library correctly declines to call it one. This is that, plus the furniture a page
// carries: navigation, a cookie banner, a script, a footer.
func articlePage() string {
	body := strings.Repeat(
		"The núcleo is the only writer of SQLite in this system, and the sidecars talk to it over "+
			"the local API rather than opening the file. ", 12)
	return `<html><head><title>Only the núcleo writes</title></head><body>
		<nav><a href="/">Home</a><a href="/about">About</a><a href="/tags">Tags</a></nav>
		<div id="cookie-banner">We value your privacy. Accept all cookies?</div>
		<script>window.dataLayer=[];</script>
		<article><h1>Only the núcleo writes</h1><p>` + body + `</p>
		<p>` + body + `</p></article>
		<footer>© 2026</footer></body></html>`
}

func TestExtractKeepsTheProseAndDropsTheFurniture(t *testing.T) {
	article, err := Extract(articlePage())
	if err != nil {
		t.Fatalf("a plain article failed to extract: %v", err)
	}
	if article.Status != StatusArticle {
		t.Errorf("status = %q, want %q", article.Status, StatusArticle)
	}
	if !strings.Contains(article.Markdown, "only writer of SQLite") {
		t.Error("the article's own prose did not survive extraction")
	}
	for _, furniture := range []string{"window.dataLayer", "Accept all cookies", "© 2026"} {
		if strings.Contains(article.Markdown, furniture) {
			t.Errorf("%q survived extraction — it is page furniture, not content", furniture)
		}
	}
}

func TestExtractCarriesTheTitle(t *testing.T) {
	article, err := Extract(articlePage())
	if err != nil {
		t.Fatalf("extract failed: %v", err)
	}
	if article.Title == "" {
		t.Error("no title was extracted")
	}
}

// The single most common thing a fetch without a browser gets back. It must not be stored as though
// it were the page — that is what makes `render: true` (spec §3.5) worth building later.
func TestExtractRefusesAJavaScriptInterstitial(t *testing.T) {
	page := `<html><head><title>Loading</title></head>
		<body><div id="root"></div><noscript>You need to enable JavaScript to run this app.</noscript>
		<script src="/bundle.js"></script></body></html>`

	if _, err := Extract(page); !errors.Is(err, ErrEmpty) {
		t.Fatalf("an empty SPA shell was accepted as content: %v", err)
	}
}

func TestExtractRefusesEmptyInput(t *testing.T) {
	for _, page := range []string{"", "   ", "\n\t "} {
		if _, err := Extract(page); !errors.Is(err, ErrEmpty) {
			t.Errorf("%q was accepted as a page: %v", page, err)
		}
	}
}

// A page whose content is navigation — an index, a search result — is a legitimate shape, not a
// failure. It falls back to the accessibility tree rather than returning nothing.
func TestExtractFallsBackForNavigationHeavyPages(t *testing.T) {
	var links strings.Builder
	for i := 0; i < 60; i++ {
		links.WriteString(`<li><a href="/post/`)
		links.WriteString(strings.Repeat("x", 3))
		links.WriteString(`">A post about the núcleo and its sidecars, number `)
		links.WriteString(strings.Repeat("y", 5))
		links.WriteString(`</a></li>`)
	}
	page := `<html><head><title>Archive</title></head><body><main><ul>` +
		links.String() + `</ul></main></body></html>`

	article, err := Extract(page)
	if err != nil {
		t.Fatalf("an index page returned nothing at all: %v", err)
	}
	if article.Status != StatusArticle && article.Status != StatusFallback {
		t.Errorf("status = %q, want article or fallback", article.Status)
	}
	if strings.TrimSpace(article.Markdown) == "" {
		t.Error("an index page extracted to an empty string")
	}
}

// The floor is what separates a real short page from a consent banner. Below it, "no readable
// content" is the honest answer.
func TestExtractRefusesContentBelowTheFloor(t *testing.T) {
	page := `<html><head><title>x</title></head><body><p>Too short.</p></body></html>`
	if _, err := Extract(page); !errors.Is(err, ErrEmpty) {
		t.Fatalf("content below MinContentChars was accepted: %v", err)
	}
}
