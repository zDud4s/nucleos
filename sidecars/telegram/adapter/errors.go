package adapter

import (
	"context"
	"errors"
	"strings"

	"nucleoschannelkit/kit"

	"nucleostelegram/telegram"
)

// Classify maps an error from a Telegram call to the kit's retryable/permanent classes, so the ack
// tells the core whether another attempt can help. A context cancellation or deadline is returned
// unchanged: kit.AckFromResult then sends no ack and the lease expires, which is the right outcome
// for a shutdown.
func Classify(err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return err
	}
	var apiErr *telegram.APIError
	if !errors.As(err, &apiErr) {
		if telegram.IsTransport(err) {
			return kit.Retryable("network", 0)
		}
		return err
	}
	desc := strings.ToLower(apiErr.Description)
	switch {
	case apiErr.StatusCode == 429 || apiErr.RetryAfter > 0:
		return kit.Retryable("rate_limited", apiErr.RetryAfter)
	case apiErr.StatusCode >= 500:
		return kit.Retryable("telegram_5xx", 0)
	case strings.Contains(desc, "message thread not found"):
		return kit.Permanent("thread_not_found")
	case strings.Contains(desc, "message to edit not found"), strings.Contains(desc, "message_id_invalid"):
		// The edit target was deleted (or never existed): no retry can help, and the core drops a
		// best-effort progress edit on this reason.
		return kit.Permanent("edit_target_gone")
	case strings.Contains(desc, "chat not found"), strings.Contains(desc, "bot was kicked"):
		return kit.Permanent("conversation_not_found")
	case apiErr.StatusCode == 401:
		return kit.Permanent("unauthorized")
	case apiErr.StatusCode == 403:
		return kit.Permanent("forbidden")
	case strings.Contains(desc, "message is too long"):
		return kit.Permanent("message_too_long")
	default:
		// Includes "can't parse entities": the caller may fall back to plain text (IsParseEntities),
		// but if it does not, the action itself is what Telegram refused.
		return kit.Permanent("invalid_action")
	}
}

// IsParseEntities reports whether Telegram refused the message because its markup did not parse.
func IsParseEntities(err error) bool {
	var apiErr *telegram.APIError
	return errors.As(err, &apiErr) && apiErr.StatusCode == 400 &&
		strings.Contains(strings.ToLower(apiErr.Description), "can't parse entities")
}
