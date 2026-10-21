package kit

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
)

const testToken = "s3cret-token"

func newTestClient(t *testing.T, h http.HandlerFunc) *Client {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return NewClient(Config{BaseURL: srv.URL, Channel: "tg", Token: testToken})
}

func testHello() contract.Hello {
	mt := 4096
	return contract.Hello{Kind: "telegram", Version: "1.0.0", Capabilities: &contract.Capabilities{MaxText: &mt, Buttons: true}}
}

func TestHelloSendsEncodedBodyAndReturnsCoreVersion(t *testing.T) {
	var gotMethod, gotPath, gotAuth string
	var gotBody []byte
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		gotMethod, gotPath, gotAuth = r.Method, r.URL.Path, r.Header.Get("Authorization")
		gotBody, _ = io.ReadAll(r.Body)
		w.Write([]byte(`{"contract_version":"1.0.0"}`))
	})
	resp, err := c.Hello(context.Background(), testHello())
	if err != nil {
		t.Fatal(err)
	}
	if *resp.ContractVersion != "1.0.0" {
		t.Errorf("version = %s", *resp.ContractVersion)
	}
	if gotMethod != http.MethodPut || gotPath != "/channels/tg/hello" || gotAuth != "Bearer "+testToken {
		t.Errorf("request = %s %s auth %q", gotMethod, gotPath, gotAuth)
	}
	if _, err := contract.DecodeStrict[contract.Hello](gotBody); err != nil {
		t.Errorf("body does not decode strictly: %v (%s)", err, gotBody)
	}
}

func TestHelloIncompatibleResponseVersion(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte(`{"contract_version":"2.0.0"}`))
	})
	if _, err := c.Hello(context.Background(), testHello()); !errors.Is(err, ErrIncompatible) {
		t.Fatalf("err = %v, want ErrIncompatible", err)
	}
}

func TestHelloConflictContractVersionIsFatal(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusConflict)
		w.Write([]byte(`{"error":"contract_version","contract_version":"1.0.0","detail":"adapter speaks 2.0.0"}`))
	})
	if _, err := c.Hello(context.Background(), testHello()); !errors.Is(err, ErrIncompatible) {
		t.Fatalf("err = %v, want ErrIncompatible", err)
	}
}

func TestHelloServerErrorIsTransientAndForbiddenIsFatal(t *testing.T) {
	for status, want := range map[int]error{500: ErrTransient, 403: ErrFatal} {
		c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(status) })
		if _, err := c.Hello(context.Background(), testHello()); !errors.Is(err, want) {
			t.Errorf("status %d: err = %v, want %v", status, err, want)
		}
	}
}

func TestHelloNetworkErrorDoesNotLeakToken(t *testing.T) {
	srv := httptest.NewServer(http.NotFoundHandler())
	addr := srv.URL
	srv.Close()
	c := NewClient(Config{BaseURL: addr, Channel: "tg", Token: testToken})
	_, err := c.Hello(context.Background(), testHello())
	if !errors.Is(err, ErrTransient) {
		t.Fatalf("err = %v", err)
	}
	if strings.Contains(err.Error(), testToken) {
		t.Errorf("token leaked: %v", err)
	}
}

func TestMediaUploadSendsRawBytesWithBareMIME(t *testing.T) {
	var gotMethod, gotPath, gotCT string
	var gotBody []byte
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		gotMethod, gotPath, gotCT = r.Method, r.URL.Path, r.Header.Get("Content-Type")
		gotBody, _ = io.ReadAll(r.Body)
		w.Write([]byte(`{"media_id":"m_17"}`))
	})
	id, err := c.UploadMedia(context.Background(), "audio/ogg; codecs=opus", bytes.NewReader([]byte("OggS")))
	if err != nil {
		t.Fatal(err)
	}
	if id != "m_17" {
		t.Errorf("id = %q", id)
	}
	if gotMethod != http.MethodPost || gotPath != "/channels/tg/media" || gotCT != "audio/ogg" || string(gotBody) != "OggS" {
		t.Errorf("request = %s %s ct %q body %q", gotMethod, gotPath, gotCT, gotBody)
	}
}

func TestMediaUploadBadIDIsFatal(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) { w.Write([]byte(`{"media_id":"17"}`)) })
	if _, err := c.UploadMedia(context.Background(), "image/png", strings.NewReader("x")); !errors.Is(err, ErrFatal) {
		t.Fatalf("err = %v", err)
	}
}

func TestMediaUploadTooLarge(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusRequestEntityTooLarge)
		w.Write([]byte(`{"error":"media_too_large","detail":"limit is 20971520 bytes"}`))
	})
	if _, err := c.UploadMedia(context.Background(), "image/png", strings.NewReader("x")); !errors.Is(err, ErrMediaTooLarge) {
		t.Fatalf("err = %v, want ErrMediaTooLarge", err)
	}
}

func TestMediaDownloadReturnsContent(t *testing.T) {
	var gotPath string
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		w.Write([]byte("12345"))
	})
	rc, err := c.DownloadMedia(context.Background(), "m_17", 5)
	if err != nil {
		t.Fatal(err)
	}
	defer rc.Close()
	data, _ := io.ReadAll(rc)
	if string(data) != "12345" || gotPath != "/channels/tg/media/m_17" {
		t.Errorf("data %q path %q", data, gotPath)
	}
}

func TestMediaDownloadOverMaxIsTooLargeNeverTruncated(t *testing.T) {
	for name, chunked := range map[string]bool{"content-length": false, "chunked": true} {
		c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
			w.Write([]byte("123456"))
			if chunked {
				w.(http.Flusher).Flush()
			}
		})
		rc, err := c.DownloadMedia(context.Background(), "m_17", 5)
		if !errors.Is(err, ErrMediaTooLarge) || rc != nil {
			t.Errorf("%s: rc=%v err=%v, want ErrMediaTooLarge", name, rc, err)
		}
	}
}

func TestMediaDownloadStatuses(t *testing.T) {
	for status, want := range map[int]error{404: ErrMediaNotFound, 403: ErrFatal, 503: ErrTransient} {
		c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(status)
			w.Write([]byte(`{"error":"channel_mismatch"}`))
		})
		if _, err := c.DownloadMedia(context.Background(), "m_1", 10); !errors.Is(err, want) {
			t.Errorf("status %d: err = %v, want %v", status, err, want)
		}
	}
}
