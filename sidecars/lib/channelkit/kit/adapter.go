package kit

import (
	"context"

	"nucleoschannelkit/contract"
)

// Adapter is the platform side of a channel: the Runner leases actions from the
// core and hands them to it.
type Adapter interface {
	// Capabilities is what the adapter declares in its hello.
	Capabilities() contract.Capabilities
	// Execute performs one action on the platform and returns the platform refs of
	// the messages it produced (none is valid: an ok without refs is a success).
	// Failures are classified with Retryable / Permanent; a cancelled ctx means
	// shutdown and produces no ack.
	Execute(ctx context.Context, a contract.Action) (platformRefs []string, err error)
	// Typing shows a typing indicator in a conversation. It is best-effort: its
	// error is only logged and nothing is acknowledged.
	Typing(ctx context.Context, conversation string) error
}
