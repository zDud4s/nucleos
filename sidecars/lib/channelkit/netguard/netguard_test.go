package netguard

import (
	"strings"
	"testing"
)

func TestIsLoopbackHost(t *testing.T) {
	cases := []struct {
		host string
		want bool
	}{
		{"localhost", true},
		{"LOCALHOST", true},
		{"LocalHost", true},
		{"127.0.0.1", true},
		{"127.0.0.2", true},
		{"::1", true},
		{"0.0.0.0", false},
		{"::", false},
		{"10.0.0.1", false},
		{"192.168.1.5", false},
		{"example.com", false},
		{"localhost.example.com", false},
		{"[::1]", false}, // brackets belong to host:port, not to a host
		{"", false},
	}
	for _, tc := range cases {
		if got := IsLoopbackHost(tc.host); got != tc.want {
			t.Errorf("IsLoopbackHost(%q) = %v, want %v", tc.host, got, tc.want)
		}
	}
}

func TestRequireLoopbackAddr(t *testing.T) {
	cases := []struct {
		addr string
		ok   bool
	}{
		{"localhost:8795", true},
		{"LOCALHOST:8795", true},
		{"127.0.0.1:8795", true},
		{"127.0.0.2:8795", true},
		{"[::1]:8795", true},
		{"::1", false}, // a bare IPv6 literal has no port
		{"0.0.0.0:8795", false},
		{"[::]:8795", false},
		{":8795", false}, // empty host binds every interface
		{"example.com:8795", false},
		{"10.0.0.1:8795", false},
		{"192.168.1.135:8795", false},
		{"1.2.3.4:8795", false},
		{"127.0.0.1", false}, // missing port
		{"8795", false},
		{"not-an-addr", false},
	}
	for _, tc := range cases {
		t.Run(tc.addr, func(t *testing.T) {
			err := RequireLoopbackAddr("TEST_ADDR", tc.addr)
			if tc.ok && err != nil {
				t.Fatalf("RequireLoopbackAddr(%q) = %v, want nil", tc.addr, err)
			}
			if !tc.ok {
				if err == nil {
					t.Fatalf("RequireLoopbackAddr(%q) = nil, want an error", tc.addr)
				}
				if !strings.Contains(err.Error(), "TEST_ADDR") {
					t.Fatalf("the error should name the variable, got %v", err)
				}
			}
		})
	}
}

func TestRequireLoopbackURL(t *testing.T) {
	cases := []struct {
		raw string
		ok  bool
	}{
		{"http://127.0.0.1:8791", true},
		{"https://127.0.0.1:8791", true},
		{"http://localhost:8787/", true},
		{"http://LOCALHOST:8787", true},
		{"http://[::1]:8787", true},
		{"ftp://127.0.0.1", false},
		{"http://", false},        // empty host
		{"http://:8791", false},   // empty host, port only
		{"http:///path", false},   // empty host, path only
		{"127.0.0.1:8791", false}, // no scheme
		{"http://10.0.0.2:8787", false},
		{"http://0.0.0.0:8787", false},
		{"http://example.com", false},
		{"http://127.0.0.1.example.com", false},
		{"://bad", false},
	}
	for _, tc := range cases {
		t.Run(tc.raw, func(t *testing.T) {
			err := RequireLoopbackURL(tc.raw)
			if tc.ok && err != nil {
				t.Fatalf("RequireLoopbackURL(%q) = %v, want nil", tc.raw, err)
			}
			if !tc.ok && err == nil {
				t.Fatalf("RequireLoopbackURL(%q) = nil, want an error", tc.raw)
			}
		})
	}
}
