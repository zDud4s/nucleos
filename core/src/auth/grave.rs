//! Grave actions: the routes a paired remote device may call only inside a fresh step-up window.
//!
//! Reaching a route is decided by `remote_device_permits`; this table adds a second question for
//! the owner device that passed it: has the owner confirmed afresh in the last
//! [`crate::remote::STEP_UP_WINDOW`]? A request with no device (the desktop shell, the control
//! token, a run key) never asks it.

use axum::http::Method;

/// Route templates (as the router matched them) that need a live step-up from a device. The
/// settings routes are not here: they are grave only when they change a command field, which the
/// handler decides (`command_change_is_gated`). `/sidecars/{name}/restart` stays out on purpose.
/// `/config/machine` is not a route any more (it answers 404), so it is not listed.
pub const GRAVE_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/backups"),
    (Method::POST, "/backup"),
    (Method::POST, "/backups/{name}/restore"),
    (Method::GET, "/api-tokens"),
    (Method::POST, "/api-tokens"),
    (Method::DELETE, "/api-tokens/{name}"),
    (Method::PUT, "/config/secrets/{key}"),
    (Method::DELETE, "/config/secrets/{key}"),
    (Method::POST, "/remote/pairing-codes"),
    (Method::GET, "/remote/devices"),
    (Method::DELETE, "/remote/devices/{id}"),
    (Method::POST, "/config/distiller"),
    (Method::POST, "/config/distiller-sources"),
    (Method::POST, "/config/capture"),
    (Method::POST, "/config/embedding"),
    (Method::POST, "/daemon/shutdown"),
    (Method::POST, "/system/shutdown"),
    (Method::POST, "/system/restart"),
    (Method::POST, "/system/storage/compact"),
    (Method::POST, "/channels/{channel}/pairing"),
    // A chat's whole working tree and conversation leave this machine in a package, and one arrives
    // in an import. Native sessions have no step-up of their own yet (task 6.5 part C).
    (Method::GET, "/assistant/chats/{chat_id}/transfer/package"),
    (Method::POST, "/assistant/chats/import"),
];

/// Whether `method` on the matched route template `route` is a grave action.
pub fn is_grave(method: &Method, route: &str) -> bool {
    GRAVE_ROUTES
        .iter()
        .any(|(grave, pattern)| grave == method && *pattern == route)
}
