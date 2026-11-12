//! Pure search over the route table.

use axum::http::Method;

use super::RouteEntry;
use crate::auth::{ApiTokenLevel, Scope, permits_route};

/// Lowercase token AND-match over method + path + purpose; path hits rank above purpose hits.
pub fn search<'a>(routes: &'a [RouteEntry], q: &str, limit: usize) -> Vec<&'a RouteEntry> {
    let tokens: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &RouteEntry)> = routes
        .iter()
        .filter_map(|entry| {
            let path = format!("{} {}", entry.method, entry.path).to_lowercase();
            let purpose = entry.purpose.to_lowercase();
            let mut in_path = 0;
            for token in &tokens {
                if path.contains(token.as_str()) {
                    in_path += 1;
                } else if !purpose.contains(token.as_str()) {
                    return None;
                }
            }
            Some((in_path, entry))
        })
        .collect();
    // Stable: ties keep table order.
    scored.sort_by_key(|(in_path, _)| std::cmp::Reverse(*in_path));
    scored.into_iter().take(limit).map(|(_, e)| e).collect()
}

/// `limit` default 10, clamped to 1..=50.
pub fn clamp_limit(limit: Option<i64>) -> usize {
    limit.unwrap_or(10).clamp(1, 50) as usize
}

/// The lowest token level `auth::permits_route` lets through; `Admin` when none below does.
pub fn authority_of(entry: &RouteEntry) -> ApiTokenLevel {
    let Ok(method) = Method::from_bytes(entry.method.as_bytes()) else {
        return ApiTokenLevel::Admin;
    };
    [ApiTokenLevel::ReadOnly, ApiTokenLevel::RunCreating]
        .into_iter()
        .find(|level| permits_route(&Scope::ApiToken(*level), &method, entry.path))
        .unwrap_or(ApiTokenLevel::Admin)
}
