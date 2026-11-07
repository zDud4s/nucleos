package serve

import (
	"fmt"
	"log/slog"
	"net/http"
	"time"
)

// tooManyRequests preserves the slow_down response and adds a passkey scope when one applies.
func tooManyRequests(w http.ResponseWriter, wait time.Duration, scope string) {
	secs := int((wait + time.Second - 1) / time.Second)
	if secs < 1 {
		secs = 1
	}
	w.Header().Set("Retry-After", fmt.Sprint(secs))
	body := map[string]string{"error": "slow_down"}
	if scope != "" {
		body["scope"] = scope
	}
	writeJSON(w, http.StatusTooManyRequests, body)
}

func (a *authHandler) globalAllowed(w http.ResponseWriter, route string) bool {
	if ok, wait, transitioned := a.pglobal.allow(0); !ok {
		if transitioned {
			slog.Warn("passkey rate limited", "scope", "global", "route", route, "retry_after", wait)
		}
		tooManyRequests(w, wait, "global")
		return false
	}
	return true
}

// deviceAllowed applies the per-device window and the failure brake to a device the daemon has
// already identified. The brake is checked first so a braked client hammering the gateway does not
// also fill its own request window.
func (a *authHandler) deviceAllowed(w http.ResponseWriter, id int64, route string) bool {
	if blocked, wait, transitioned := a.pfail.blocked(id); blocked {
		if transitioned {
			slog.Warn("passkey rate limited", "scope", "device", "device_id", id, "route", route, "retry_after", wait)
		}
		tooManyRequests(w, wait, "device")
		return false
	}
	if ok, wait, transitioned := a.pdev.allow(id); !ok {
		if transitioned {
			slog.Warn("passkey rate limited", "scope", "device", "device_id", id, "route", route, "retry_after", wait)
		}
		tooManyRequests(w, wait, "device")
		return false
	}
	return true
}

// finishFailure records a failed finish. The brake transition is logged by blocked(), the first
// time a request is refused, because allow() only flags a bucket when it refuses an event.
func (a *authHandler) finishFailure(id int64) {
	_, _, _ = a.pfail.allow(id)
}

// finishSuccess clears the brake. A braked device is refused before it can reach a finish handler,
// so this only matters while the device is still below the failure limit.
func (a *authHandler) finishSuccess(id int64) {
	a.pfail.reset(id)
}
