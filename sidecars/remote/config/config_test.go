package config

import (
	"strings"
	"testing"
	"time"
)

func env(overrides map[string]string) func(string) string {
	base := map[string]string{
		EnvDaemonURL:    "http://127.0.0.1:8787",
		EnvDaemonToken:  "remote.secret",
		EnvPublicOrigin: "https://box.tail1234.ts.net",
		EnvDistDir:      "/srv/dist-remote",
	}
	for k, v := range overrides {
		base[k] = v
	}
	return func(k string) string { return base[k] }
}

func TestConfigDefaults(t *testing.T) {
	cfg, err := LoadFrom(env(nil))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Addr != "127.0.0.1:8797" || cfg.StreamMax != 120*time.Second ||
		cfg.CookieMaxAge != 2592000 || cfg.MaxBodyBytes != 64<<20 {
		t.Errorf("unexpected defaults: %+v", cfg)
	}
	if cfg.PublicOrigin != "https://box.tail1234.ts.net" || cfg.PublicHost != "box.tail1234.ts.net" {
		t.Errorf("origin not normalised: %q %q", cfg.PublicOrigin, cfg.PublicHost)
	}
}

func TestConfigRefusesNonLoopbackAddrAndDaemon(t *testing.T) {
	cases := map[string]map[string]string{
		"wildcard addr":     {EnvAddr: "0.0.0.0:8797"},
		"lan addr":          {EnvAddr: "192.168.1.5:8797"},
		"empty-host addr":   {EnvAddr: ":8797"},
		"public daemon":     {EnvDaemonURL: "http://10.0.0.2:8787"},
		"named daemon":      {EnvDaemonURL: "http://example.com"},
		"ftp daemon":        {EnvDaemonURL: "ftp://127.0.0.1"},
		"http origin":       {EnvPublicOrigin: "http://box.ts.net"},
		"origin with path":  {EnvPublicOrigin: "https://box.ts.net/app"},
		"origin with query": {EnvPublicOrigin: "https://box.ts.net/?a=1"},
		"missing token":     {EnvDaemonToken: ""},
		"missing dist":      {EnvDistDir: ""},
		"missing daemon":    {EnvDaemonURL: ""},
		"missing origin":    {EnvPublicOrigin: ""},
		"stream too short":  {EnvStreamMax: "5"},
		"stream too long":   {EnvStreamMax: "901"},
		"stream not number": {EnvStreamMax: "soon"},
	}
	for name, over := range cases {
		if _, err := LoadFrom(env(over)); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
	for _, ok := range []map[string]string{
		{EnvAddr: "localhost:9000"},
		{EnvAddr: "[::1]:8797"},
		{EnvDaemonURL: "http://localhost:8787/"},
		{EnvDaemonURL: "http://[::1]:8787"},
	} {
		if _, err := LoadFrom(env(ok)); err != nil {
			t.Errorf("%v refused: %v", ok, err)
		}
	}
	if _, err := LoadFrom(env(map[string]string{EnvAddr: "0.0.0.0:1"})); err == nil ||
		!strings.Contains(err.Error(), EnvAddr) {
		t.Errorf("error should name the variable, got %v", err)
	}
}
