//! §spec browser-integrado
//!
//! Pure helpers for the agent browser environment: the Chromium arguments that route the agent
//! profile through the proxy and expose CDP, the loopback port picker, the rebuild decision, and
//! the naming of keepalive tabs and per-generation profile directories. Nothing here touches tauri.

/// Prefix of the tab id of a project's keepalive tab.
pub(crate) const KEEPALIVE_TAB_PREFIX: &str = "agent-keepalive-";

/// Longest tab id accepted.
const MAX_TAB_ID_LEN: usize = 64;

/// The browser arguments of the agent environment: proxy, CDP port and the CDP origin allow-list.
pub(crate) fn agent_args(proxy_addr: &str, port: u16) -> Vec<String> {
    vec![
        format!("--proxy-server=http://{proxy_addr}"),
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=http://127.0.0.1:1".to_string(),
    ]
}

/// A free TCP port on the loopback interface. The listener is dropped before returning, so the
/// port is only free at that instant.
pub(crate) fn free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("port_unavailable: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("port_unavailable: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// What to do with an existing agent environment when a configuration is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RebuildDecision {
    Reuse,
    Create,
    Rebuild,
}

/// `None` creates, an identical configuration is reused, a different one rebuilds.
pub(crate) fn rebuild_decision(existing: Option<&str>, requested: &str) -> RebuildDecision {
    match existing {
        None => RebuildDecision::Create,
        Some(e) if e == requested => RebuildDecision::Reuse,
        Some(_) => RebuildDecision::Rebuild,
    }
}

/// The keepalive tab id of a project. The project must be usable in a Tauri label and a tab id:
/// non-empty, ASCII alphanumerics, `-` or `_`, and the resulting id at most 64 chars.
pub(crate) fn keepalive_tab_id(project: &str) -> Result<String, String> {
    let valid = !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let id = format!("{KEEPALIVE_TAB_PREFIX}{project}");
    if !valid || id.len() > MAX_TAB_ID_LEN {
        return Err("invalid_project".to_string());
    }
    Ok(id)
}

/// The webview label of a project's keepalive tab. The `browser-` prefix must agree with
/// `LABEL_PREFIX` in `browser.rs`.
pub(crate) fn keepalive_label(project: &str) -> Result<String, String> {
    Ok(format!("browser-{}", keepalive_tab_id(project)?))
}

/// Whether a tab id names a keepalive tab.
pub(crate) fn is_keepalive_tab_id(tab_id: &str) -> bool {
    tab_id.starts_with(KEEPALIVE_TAB_PREFIX)
}

/// Directory name of the agent profile for a generation: `agent`, then `agent-1`, `agent-2`, ...
pub(crate) fn generation_dir_name(generation: u32) -> String {
    if generation == 0 {
        "agent".to_string()
    } else {
        format!("agent-{generation}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_args_are_exact() {
        assert_eq!(
            agent_args("127.0.0.1:8080", 9222),
            vec![
                "--proxy-server=http://127.0.0.1:8080".to_string(),
                "--remote-debugging-port=9222".to_string(),
                "--remote-allow-origins=http://127.0.0.1:1".to_string(),
            ]
        );
    }

    #[test]
    fn free_port_is_nonzero_and_rebindable() {
        let port = free_loopback_port().unwrap();
        assert_ne!(port, 0);
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        assert!(free_loopback_port().is_ok());
        assert!(free_loopback_port().is_ok());
    }

    #[test]
    fn rebuild_decision_cases() {
        assert_eq!(rebuild_decision(None, "a"), RebuildDecision::Create);
        assert_eq!(rebuild_decision(Some("a"), "a"), RebuildDecision::Reuse);
        assert_eq!(rebuild_decision(Some("a"), "b"), RebuildDecision::Rebuild);
    }

    #[test]
    fn keepalive_id_and_label() {
        assert_eq!(keepalive_tab_id("p1").unwrap(), "agent-keepalive-p1");
        assert_eq!(keepalive_label("p1").unwrap(), "browser-agent-keepalive-p1");
    }

    #[test]
    fn keepalive_rejects_bad_projects() {
        for bad in ["", "a b", "a/b"] {
            assert_eq!(keepalive_tab_id(bad), Err("invalid_project".to_string()));
            assert_eq!(keepalive_label(bad), Err("invalid_project".to_string()));
        }
        let long = "x".repeat(60);
        assert_eq!(keepalive_tab_id(&long), Err("invalid_project".to_string()));
    }

    #[test]
    fn keepalive_tab_id_detection() {
        assert!(is_keepalive_tab_id("agent-keepalive-p1"));
        assert!(!is_keepalive_tab_id("tab-1"));
    }

    #[test]
    fn generation_dir_names() {
        assert_eq!(generation_dir_name(0), "agent");
        assert_eq!(generation_dir_name(1), "agent-1");
        assert_eq!(generation_dir_name(7), "agent-7");
    }
}
