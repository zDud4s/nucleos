package kit

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"

	"nucleoschannelkit/clock"
)

// Config configures a Client.
type Config struct {
	BaseURL string // core base URL, e.g. http://127.0.0.1:7777
	Channel string // channel id used in /channels/{channel}/* routes
	Token   string // bearer token, read from the environment by the caller
	// HTTP is the transport. Nil means a client with no global Timeout:
	// long-polls and media transfers set deadlines per request via context.
	HTTP  *http.Client
	Clock clock.Clock // nil means the real clock
}

// Client is the base HTTP client of the contract.
type Client struct {
	baseURL string
	channel string
	token   string
	http    *http.Client
	clock   clock.Clock
	stats   *Stats // optional, set by WithStats
}

// NewClient builds a Client, filling the defaults.
func NewClient(cfg Config) *Client {
	h := cfg.HTTP
	if h == nil {
		h = &http.Client{}
	}
	clk := cfg.Clock
	if clk == nil {
		clk = clock.Real()
	}
	return &Client{
		baseURL: strings.TrimRight(cfg.BaseURL, "/"),
		channel: cfg.Channel,
		token:   cfg.Token,
		http:    h,
		clock:   clk,
	}
}

// do performs one request and returns the body and status. A transport error
// returns status 0 and an error with the token redacted; the error wraps the
// cause (so context cancellation stays detectable).
func (c *Client) do(ctx context.Context, method, path string, body io.Reader, contentType string) ([]byte, int, error) {
	req, err := http.NewRequestWithContext(ctx, method, c.baseURL+path, body)
	if err != nil {
		return nil, 0, c.redact(fmt.Errorf("create request: %w", err))
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	if contentType != "" {
		req.Header.Set("Content-Type", contentType)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, 0, c.redact(fmt.Errorf("perform request: %w", err))
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, resp.StatusCode, c.redact(fmt.Errorf("read response: %w", err))
	}
	return data, resp.StatusCode, nil
}

// redactedError hides the token in the message while keeping the cause chain.
type redactedError struct {
	msg   string
	cause error
}

func (e *redactedError) Error() string { return e.msg }
func (e *redactedError) Unwrap() error { return e.cause }

func (c *Client) redact(err error) error {
	if c.token == "" {
		return err
	}
	msg := err.Error()
	if !strings.Contains(msg, c.token) {
		return err
	}
	return &redactedError{msg: strings.ReplaceAll(msg, c.token, "[redacted]"), cause: err}
}
