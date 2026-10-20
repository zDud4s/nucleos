package main

import (
	"strings"
	"testing"
)

const testBotToken = "123456:SECRET-bot-token-value"

func envOf(m map[string]string) func(string) string {
	return func(k string) string { return m[k] }
}

func baseEnv() map[string]string {
	return map[string]string{
		"NUCLEOS_DAEMON_URL":    "http://127.0.0.1:7777",
		"NUCLEOS_CHANNEL_TOKEN": "chan-token",
		"TELEGRAM_BOT_TOKEN":    testBotToken,
	}
}

func TestConfigMissingBotTokenIsAnError(t *testing.T) {
	env := baseEnv()
	delete(env, "TELEGRAM_BOT_TOKEN")
	if _, err := loadConfig(envOf(env)); err == nil {
		t.Fatal("expected an error without TELEGRAM_BOT_TOKEN")
	}
}

func TestConfigErrorsNeverCarryTheBotToken(t *testing.T) {
	cases := map[string]func(map[string]string){
		"no daemon url":     func(m map[string]string) { delete(m, "NUCLEOS_DAEMON_URL") },
		"non-loopback url":  func(m map[string]string) { m["NUCLEOS_DAEMON_URL"] = "http://example.com" },
		"no channel token":  func(m map[string]string) { delete(m, "NUCLEOS_CHANNEL_TOKEN") },
		"no bot token":      func(m map[string]string) { delete(m, "TELEGRAM_BOT_TOKEN") },
		"blank bot token":   func(m map[string]string) { m["TELEGRAM_BOT_TOKEN"] = "  " },
	}
	for name, mutate := range cases {
		env := baseEnv()
		mutate(env)
		_, err := loadConfig(envOf(env))
		if err == nil {
			t.Fatalf("%s: expected an error", name)
		}
		if strings.Contains(err.Error(), testBotToken) || strings.Contains(err.Error(), "SECRET") {
			t.Fatalf("%s: error leaks the bot token: %v", name, err)
		}
	}
}

func TestConfigOffsetPathFromEnv(t *testing.T) {
	env := baseEnv()
	env["NUCLEOS_TELEGRAM_OFFSET"] = "/tmp/custom-offset"
	cfg, err := loadConfig(envOf(env))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.OffsetPath != "/tmp/custom-offset" {
		t.Fatalf("offset path = %q", cfg.OffsetPath)
	}
}

func TestConfigOffsetPathDefaultsToTheV1File(t *testing.T) {
	cfg, err := loadConfig(envOf(baseEnv()))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasSuffix(cfg.OffsetPath, "telegram-offset") {
		t.Fatalf("offset path = %q", cfg.OffsetPath)
	}
}

func TestConfigChannelIDDefaultsToTelegram(t *testing.T) {
	cfg, err := loadConfig(envOf(baseEnv()))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Kit.Channel != "telegram" {
		t.Fatalf("channel = %q", cfg.Kit.Channel)
	}
	if cfg.BotToken != testBotToken {
		t.Fatal("bot token not carried")
	}

	env := baseEnv()
	env["NUCLEOS_CHANNEL_ID"] = "tg-main"
	cfg, err = loadConfig(envOf(env))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Kit.Channel != "tg-main" {
		t.Fatalf("channel = %q", cfg.Kit.Channel)
	}
}

func TestConfigKindIsTelegram(t *testing.T) {
	cfg, err := loadConfig(envOf(baseEnv()))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Kit.Kind != "telegram" {
		t.Fatalf("kit kind = %q, want telegram", cfg.Kit.Kind)
	}
}
