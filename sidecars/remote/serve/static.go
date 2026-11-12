package serve

import (
	"bytes"
	"io"
	"net/http"
	"path"
	"strings"

	"nucleosremote/config"
)

// noncePlaceholder is what the built index.html carries in its <style nonce="..."> tag.
const noncePlaceholder = "__CSP_NONCE__"

// maxIndexBytes bounds index.html, which is read into memory to substitute the nonce.
const maxIndexBytes = 4 << 20

// staticHandler serves the built web shell from the dist directory.
type staticHandler struct {
	fs http.FileSystem
}

// NewStatic builds the handler for everything outside /api and /auth.
func NewStatic(cfg config.Config) http.Handler {
	return &staticHandler{fs: http.Dir(cfg.DistDir)}
}

func (s *staticHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		w.Header().Set("Allow", "GET, HEAD")
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return
	}
	p := path.Clean("/" + r.URL.Path)
	for _, seg := range strings.Split(p, "/") {
		// Dotfiles and anything resembling traversal are never served.
		if strings.HasPrefix(seg, ".") {
			writeJSONError(w, http.StatusNotFound, "not_found")
			return
		}
	}
	if p == "/" || p == "/index.html" {
		s.index(w, r)
		return
	}
	f, err := s.fs.Open(p)
	if err == nil {
		defer f.Close()
		if st, serr := f.Stat(); serr == nil && st.Mode().IsRegular() {
			http.ServeContent(w, r, st.Name(), st.ModTime(), f)
			return
		}
		// A directory is never listed; it falls through like a missing route.
	}
	if path.Ext(p) == "" {
		// A client-side route: the app resolves it.
		s.index(w, r)
		return
	}
	writeJSONError(w, http.StatusNotFound, "not_found")
}

// index serves index.html with a fresh CSP nonce in place of the placeholder.
func (s *staticHandler) index(w http.ResponseWriter, r *http.Request) {
	nonce := NonceFrom(r.Context())
	if nonce == "" {
		// Outside the security layer there is no nonce to match the policy against.
		writeJSONError(w, http.StatusInternalServerError, "no_nonce")
		return
	}
	f, err := s.fs.Open("/index.html")
	if err != nil {
		writeJSONError(w, http.StatusNotFound, "not_found")
		return
	}
	defer f.Close()
	raw, err := io.ReadAll(io.LimitReader(f, maxIndexBytes))
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "unreadable")
		return
	}
	page := bytes.ReplaceAll(raw, []byte(noncePlaceholder), []byte(nonce))
	h := w.Header()
	h.Set("Content-Type", "text/html; charset=utf-8")
	h.Set("Cache-Control", "no-cache")
	if r.Method == http.MethodHead {
		return
	}
	_, _ = w.Write(page)
}
