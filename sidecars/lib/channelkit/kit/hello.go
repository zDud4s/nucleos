package kit

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"

	"nucleoschannelkit/contract"
)

var (
	// ErrIncompatible: the contract versions of the adapter and the core do not match; the adapter must terminate.
	ErrIncompatible = errors.New("channelkit: incompatible contract version")
	// ErrFatal: misconfiguration or rejection that retrying cannot fix; the adapter must terminate.
	ErrFatal = errors.New("channelkit: fatal response")
	// ErrTransient: a network error or a retryable status; try again with backoff.
	ErrTransient = errors.New("channelkit: transient failure")
)

// channelPath builds /channels/{channel}/suffix with the channel escaped.
func (c *Client) channelPath(suffix string) string {
	return "/channels/" + url.PathEscape(c.channel) + suffix
}

// Hello presents the adapter to the core (PUT /channels/{channel}/hello) and
// returns the core's contract version. An incompatible version, announced by a
// 409 contract_version or by the response itself, yields ErrIncompatible.
func (c *Client) Hello(ctx context.Context, hello contract.Hello) (contract.HelloResponse, error) {
	var zero contract.HelloResponse
	payload, err := json.Marshal(hello)
	if err != nil {
		return zero, fmt.Errorf("encode hello: %w", err)
	}
	body, status, err := c.do(ctx, http.MethodPut, c.channelPath("/hello"), bytes.NewReader(payload), "application/json")
	if err != nil {
		return zero, fmt.Errorf("%w: hello: %w", ErrTransient, err)
	}
	switch Classify(EndpointHello, status, body) {
	case OK:
	case Retry:
		return zero, fmt.Errorf("%w: hello: status %d", ErrTransient, status)
	default:
		if status == http.StatusConflict && errorCode(body) == "contract_version" {
			return zero, fmt.Errorf("%w: rejected by the core", ErrIncompatible)
		}
		return zero, fmt.Errorf("%w: hello: status %d", ErrFatal, status)
	}
	resp, err := contract.DecodeLenient[contract.HelloResponse](body)
	if err != nil {
		return zero, fmt.Errorf("%w: hello response: %w", ErrFatal, err)
	}
	if !contract.Compatible(*resp.ContractVersion) {
		return zero, fmt.Errorf("%w: core speaks %s", ErrIncompatible, *resp.ContractVersion)
	}
	return resp, nil
}
