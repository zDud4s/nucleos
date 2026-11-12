package serve

import "testing"

const downloadURL = "/api/files/download?path=a.txt"

func TestDownloadExemptionAllowsOnlySameOriginGet(t *testing.T) {
	h := handler()
	rec := do(h, "GET", downloadURL, map[string]string{"Sec-Fetch-Site": "same-origin"})
	if rec.Code != 200 {
		t.Fatalf("same-origin GET without the CSRF header: got %d, want 200", rec.Code)
	}
	// A foreign Origin header is still refused.
	rec = do(h, "GET", downloadURL, map[string]string{"Sec-Fetch-Site": "same-origin", "Origin": "https://evil.example"})
	if rec.Code != 403 {
		t.Fatalf("foreign Origin: got %d, want 403", rec.Code)
	}
}

func TestDownloadExemptionRefusesCrossSite(t *testing.T) {
	h := handler()
	for _, site := range []string{"cross-site", "same-site", "none"} {
		rec := do(h, "GET", downloadURL, map[string]string{"Sec-Fetch-Site": site})
		if rec.Code != 403 {
			t.Errorf("Sec-Fetch-Site %q: got %d, want 403", site, rec.Code)
		}
	}
}

func TestDownloadExemptionRefusesMissingFetchMetadata(t *testing.T) {
	rec := do(handler(), "GET", downloadURL, nil)
	if rec.Code != 403 {
		t.Fatalf("no Sec-Fetch-Site: got %d, want 403", rec.Code)
	}
}

func TestDownloadExemptionCoversNoOtherPath(t *testing.T) {
	h := handler()
	for _, p := range []string{
		"/api/files/download/../secrets",
		"/api/files/download/",
		"/api/files/downloads",
		"/api/files/download/x",
		"/api/files/list",
		"/api//files/download",
		"/api/files/../files/download",
		"/api/x",
	} {
		rec := do(h, "GET", p, map[string]string{"Sec-Fetch-Site": "same-origin"})
		// The mux redirects an unclean path (307) before the guard; either way the handler never runs.
		if rec.Code == 200 {
			t.Errorf("%s: got 200, want a refusal or redirect", p)
		}
	}
	// The guard itself, without the mux: an unclean path is not exempt.
	for _, p := range []string{"/api/files/download/../secrets", "/api/files/download/"} {
		if rec := do(guard(Policy{PublicOrigin: testOrigin, PublicHost: testHost}, true, ok200), "GET", p,
			map[string]string{"Sec-Fetch-Site": "same-origin"}); rec.Code != 403 {
			t.Errorf("guard %s: got %d, want 403", p, rec.Code)
		}
	}
}

func TestDownloadExemptionRefusesPost(t *testing.T) {
	h := handler()
	for _, m := range []string{"POST", "PUT", "DELETE", "HEAD"} {
		rec := do(h, m, downloadURL, map[string]string{"Sec-Fetch-Site": "same-origin"})
		if rec.Code != 403 {
			t.Errorf("%s: got %d, want 403", m, rec.Code)
		}
	}
}
