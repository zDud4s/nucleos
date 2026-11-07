//! Remote-device authentication routes and their admission rule.
//!
//! The segment policy remains beside the other token policy in `auth.rs`. This module owns the
//! route exceptions, paired-device admission and request extension so that the auth middleware
//! does not also carry the remote protocol's mechanics.

use axum::http::Method;

use super::{
    ApiTokenLevel, CHAT_BROWSER_AGENT_ROUTE, REMOTE_ADMIN_SEGMENTS, REMOTE_ALLOWED_SEGMENTS,
    REMOTE_OWNER_SEGMENTS, Scope, Target, grants,
};

/// The header the remote gateway forwards a browser's session secret in. Never logged.
pub const DEVICE_SESSION_HEADER: &str = "x-nucleos-device-session";

/// What the gateway's key reaches on its own, with no device session: the pairing exchange, which
/// is the one call made before a browser has a session, and the three calls of the passkey
/// ceremony, which the gateway runs for an owner device (it names the device itself, from the
/// answer to `/remote/session`). A device session never reaches these: `remote` is not in any
/// segment list a device may use.
pub(super) const REMOTE_GATEWAY_ROUTES: &[(Method, &str)] = &[
    (Method::POST, "/remote/pair"),
    (Method::GET, "/remote/passkeys/{device}"),
    (Method::POST, "/remote/passkeys"),
    (Method::POST, "/remote/step-up"),
    (Method::GET, "/remote/step-up/ticket/{ticket}"),
    (Method::POST, "/remote/step-up/ticket/consume"),
];

/// Open to every paired device whatever its level: reading itself and logging out.
const REMOTE_SELF_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/remote/session"),
    (Method::POST, "/remote/logout"),
    (Method::POST, "/stepup/ticket"),
    (Method::GET, "/stepup/status"),
];

/// Routes inside admin segments that no device reaches, whatever its level. `POST
/// /assistant/browser-agents` starts a browser on the host, the host-only surface the `browser`
/// deny covers (listing linked browser conversations, `GET`, stays open). `/email/incoming` and
/// `/email/cursor` are the email sidecar's ingestion surface, not a working surface: a device
/// reaching them could inject forged inbound mail into triage.
pub(super) const REMOTE_DEVICE_REFUSED_ROUTES: &[(Method, &str)] = &[
    (Method::POST, CHAT_BROWSER_AGENT_ROUTE),
    (Method::POST, "/email/incoming"),
    (Method::GET, "/email/cursor"),
];

/// What a request carried by a paired remote device may reach. Matched against the route template.
///
/// 1. [`REMOTE_SELF_ROUTES`] are open to every level.
/// 2. A first segment in [`REMOTE_ALLOWED_SEGMENTS`] is graded as the API token of the same level
///    (`grants`): read-only gets `READ_ONLY_ROUTES`, run-creating adds `RUN_CREATING_ROUTES`,
///    admin gets everything under the segment.
/// 3. A first segment in [`REMOTE_ADMIN_SEGMENTS`] is reachable only by an Admin device, graded as
///    the Admin token (owner decision 2026-10-29: the owner working from another machine).
/// 4. Anything else is refused, whatever the level: the hard-deny set in
///    `REMOTE_DENIED_SEGMENTS` (shutdown/update, machine settings and secrets, backups,
///    credential minting, sidecar control, messaging identity pairing, the remote owner surface,
///    host-only live browser pixels) and any unlisted family.
///
/// A few routes inside admin segments stay refused: see [`REMOTE_DEVICE_REFUSED_ROUTES`].
///
/// `owner` is the owner device flag; it only counts at the Admin level. An owner device also
/// reaches [`REMOTE_OWNER_SEGMENTS`] and the routes in [`REMOTE_DEVICE_REFUSED_ROUTES`] (it is the
/// owner), graded as the Admin token. It never reaches a segment in none of the lists. The owner
/// branch comes before the allow-list grading and before the refused routes.
pub fn remote_device_permits(
    level: ApiTokenLevel,
    owner: bool,
    method: &Method,
    route: &str,
) -> bool {
    let owner = owner && level == ApiTokenLevel::Admin;
    // The gateway's own routes are the gateway's alone, whatever the device. The owner branch
    // below would otherwise open them (`remote` is an owner segment), and a stolen owner session
    // could then grant itself a step-up or enrol its own passkey.
    if REMOTE_GATEWAY_ROUTES
        .iter()
        .any(|(gateway, pattern)| gateway == method && *pattern == route)
    {
        return false;
    }
    if REMOTE_SELF_ROUTES
        .iter()
        .any(|(allowed, pattern)| allowed == method && *pattern == route)
    {
        return true;
    }
    let first = route
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    if owner && (REMOTE_OWNER_SEGMENTS.contains(&first) || REMOTE_ADMIN_SEGMENTS.contains(&first)) {
        return grants(
            &Scope::ApiToken(ApiTokenLevel::Admin),
            method,
            route,
            Target::Route,
        );
    }
    if REMOTE_DEVICE_REFUSED_ROUTES
        .iter()
        .any(|(refused, pattern)| refused == method && *pattern == route)
    {
        return false;
    }
    if REMOTE_ALLOWED_SEGMENTS.contains(&first) {
        return grants(&Scope::ApiToken(level), method, route, Target::Route);
    }
    if level == ApiTokenLevel::Admin && REMOTE_ADMIN_SEGMENTS.contains(&first) {
        return grants(
            &Scope::ApiToken(ApiTokenLevel::Admin),
            method,
            route,
            Target::Route,
        );
    }
    false
}

/// Request extension set when the caller is a paired remote device: which one. Handlers that need
/// to know (logout) read this instead of trusting a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteDevice {
    pub id: i64,
    /// An owner device (Admin level with the owner flag): see [`REMOTE_OWNER_SEGMENTS`].
    pub owner: bool,
    /// The session holds a live step-up (`remote_devices.stepup_until` in the future), read by
    /// `remote::authenticate` on this very request. Grave routes need it ([`crate::auth::GRAVE_ROUTES`])
    /// and so does a settings command-field change.
    pub stepped_up: bool,
}
