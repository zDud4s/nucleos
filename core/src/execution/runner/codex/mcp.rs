//! The Codex CLI's MCP surface: the overrides that translate the daemon's MCP config, the
//! environment registry servers bring, and the deny hook and features an `McpOnly` turn uses.

/// The prefix Codex gives the daemon's own MCP tools: `mcp__<server>__<tool>`, server `nucleos`.
pub const DAEMON_MCP_TOOL_PREFIX: &str = "mcp__nucleos__";

/// The Codex features an `McpOnly` turn switches off (`-c features.<name>=false`).
pub const CODEX_MCP_ONLY_DISABLED_FEATURES: [&str; 10] = [
    "shell_tool",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "image_generation",
    "apps",
    "plugins",
    "tool_suggest",
    "multi_agent",
];

/// The reason the deny hook prints. No character in it is syntax to `sh` or `cmd`.
pub const CODEX_MCP_ONLY_DENIAL: &str = "nucleos: this chat may only call the nucleos MCP tools";

/// A regex over tool names that matches every name NOT starting with `prefix`: for each position
/// `i`, the proper prefix `prefix[..i]` itself, or `prefix[..i]` followed by any character other
/// than `prefix[i]`. No lookahead, so it behaves the same under search and under full-match.
pub fn codex_deny_matcher(prefix: &str) -> String {
    assert!(
        prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "the deny matcher prefix must be ASCII alphanumerics and underscores: {prefix}"
    );
    let chars: Vec<char> = prefix.chars().collect();
    let mut branches = Vec::new();
    for i in 0..chars.len() {
        let head: String = chars[..i].iter().collect();
        branches.push(head.clone());
        branches.push(format!("{head}[^{}].*", chars[i]));
    }
    format!("(?s)^(?:{})$", branches.join("|"))
}

/// The `-c hooks.PreToolUse=[...]` override that blocks every tool outside the daemon's own MCP
/// tools. Only exit code 2 plus stderr blocks a tool call; anything else (a missing interpreter,
/// a parse error, a crash) fails open, hence shell builtins only and no interpreter. Codex runs a
/// hook through the session's shell: `$SHELL -c` (or bash/zsh) on Unix, and PowerShell, never cmd,
/// on Windows (`shell_detect::default_user_shell`, 0.144.4). Measured live: cmd syntax there was a
/// PowerShell parse error, so the hook failed open and every tool ran.
///
/// KNOWN LIMITATION: Codex emits no init event, so the surviving tool set cannot be verified as
/// the Claude path does. Measured live on 0.144.4: with the feature flags off, `apply_patch`,
/// `view_image`, `update_plan`, `request_user_input`, the goal tools, the MCP resource tools and
/// `multi_agent_v1__*` remain, and this hook blocks each of them, including when called from
/// inside code mode (`functions.exec`). Code mode itself is not a hooked tool; it computes, and any
/// I/O it attempts goes through a hooked tool.
pub fn codex_deny_hook_override(prefix: &str) -> String {
    fn json(value: &str) -> String {
        serde_json::to_string(value).expect("serializing a string cannot fail")
    }
    let unix = format!("echo {CODEX_MCP_ONLY_DENIAL} >&2; exit 2");
    let windows = format!("[Console]::Error.WriteLine('{CODEX_MCP_ONLY_DENIAL}'); exit 2");
    format!(
        "hooks.PreToolUse=[{{matcher={},hooks=[{{type=\"command\",command={},commandWindows={}}}]}}]",
        json(&codex_deny_matcher(prefix)),
        json(&unix),
        json(&windows),
    )
}

/// Translates the daemon's stdio MCP configuration into Codex overrides.
/// `codex exec` runs with approval policy `never`, so Codex immediately declines an MCP tool call
/// that needs confirmation ("user cancelled MCP tool call" on 0.144.4). `approve` pre-approves
/// the daemon's own MCP servers, as the Claude path does with `--allowedTools mcp__nucleos__*`.
pub fn codex_mcp_overrides(
    config: &serde_json::Value,
    env_names: &[String],
) -> Result<Vec<String>, String> {
    let servers = config
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "mcp_config must contain an mcpServers object".to_string())?;
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();

    let mut overrides = Vec::new();
    for name in names {
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(format!(
                "mcp_config server name {name:?} cannot be expressed"
            ));
        }
        let server = servers
            .get(name)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("mcp_config server {name:?} must be an object"))?;
        if !matches!(
            server.get("type").and_then(serde_json::Value::as_str),
            None | Some("stdio")
        ) {
            return Err(format!("mcp_config server {name:?} must be stdio"));
        }
        let command = server
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("mcp_config server {name:?} must have a string command"))?;
        let args = match server.get("args") {
            None => Vec::new(),
            Some(args) => args
                .as_array()
                .and_then(|args| {
                    args.iter()
                        .map(|arg| arg.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| format!("mcp_config server {name:?} args must be a string array"))?,
        };
        overrides.push(format!(
            "mcp_servers.{name}.command={}",
            serde_json::to_string(command).expect("serializing a string cannot fail")
        ));
        overrides.push(format!(
            "mcp_servers.{name}.args={}",
            serde_json::to_string(&args).expect("serializing strings cannot fail")
        ));
        // A registry server brings its own `env` object (spec §8.2): Codex is told only its NAMES
        // and reads the values from the child environment, where `codex_mcp_env` puts them. A
        // server without one (nucleos) forwards the run's own environment, by name as well.
        let own_env_names: Option<Vec<&String>> = server
            .get("env")
            .and_then(serde_json::Value::as_object)
            .map(|env| {
                let mut names: Vec<&String> = env.keys().collect();
                names.sort();
                names
            });
        let listed = match own_env_names {
            Some(names) if names.is_empty() => None,
            Some(names) => {
                Some(serde_json::to_string(&names).expect("serializing strings cannot fail"))
            }
            None if env_names.is_empty() => None,
            None => {
                Some(serde_json::to_string(env_names).expect("serializing strings cannot fail"))
            }
        };
        if let Some(listed) = listed {
            overrides.push(format!("mcp_servers.{name}.env_vars={listed}"));
        }
        // Registry servers are pre-approved too, for the reason above: under approval `never` an
        // unapproved server's every call would be declined. The registry is the owner's own list.
        overrides.push(format!(
            "mcp_servers.{name}.default_tools_approval_mode={}",
            serde_json::to_string("approve").expect("serializing a string cannot fail")
        ));
    }
    Ok(overrides)
}

/// Every `(name, value)` of every server's `env` object in an MCP config, sorted by name.
///
/// These are the values `codex_mcp_overrides` only names: they go into the child environment and
/// nowhere else. A name already in `reserved` (the run's own environment) refuses the launch
/// rather than letting a registry server shadow a daemon variable, and so does a name in
/// `MCP_ENV_DENYLIST` (variables the Codex child itself depends on). Names compare
/// case-insensitively on Windows, where `Path` and `PATH` are one variable. Errors carry names,
/// never a value.
pub fn codex_mcp_env(
    config: &serde_json::Value,
    reserved: &[String],
) -> Result<Vec<(String, String)>, String> {
    codex_mcp_env_with(config, reserved, cfg!(windows))
}

/// Variables a registry server may never set in the Codex child's environment: the child's own
/// process plumbing and credentials. Matched case-insensitively on every platform.
const MCP_ENV_DENYLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "CODEX_HOME",
    "OPENAI_API_KEY",
    "SYSTEMROOT",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "COMSPEC",
];

fn env_name_eq(a: &str, b: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// `codex_mcp_env` with the collision check's case rule made explicit, so both rules are testable
/// on any platform.
fn codex_mcp_env_with(
    config: &serde_json::Value,
    reserved: &[String],
    case_insensitive: bool,
) -> Result<Vec<(String, String)>, String> {
    let Some(servers) = config
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(Vec::new());
    };
    let mut pairs = Vec::new();
    for (server, entry) in servers {
        let Some(env) = entry.get("env").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (name, value) in env {
            if MCP_ENV_DENYLIST
                .iter()
                .any(|denied| env_name_eq(denied, name, true))
            {
                return Err(format!(
                    "mcp_config server {server:?} env {name:?} collides with a variable the Codex child needs"
                ));
            }
            if reserved
                .iter()
                .any(|taken| env_name_eq(taken, name, case_insensitive))
            {
                return Err(format!(
                    "mcp_config server {server:?} env {name:?} collides with the daemon's environment"
                ));
            }
            let value = value.as_str().ok_or_else(|| {
                format!("mcp_config server {server:?} env {name:?} must be a string")
            })?;
            pairs.push((name.clone(), value.to_string()));
        }
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(window) = pairs.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(format!(
            "mcp_config env {:?} is set by more than one server",
            window[0].0
        ));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_a_registry_server() -> serde_json::Value {
        serde_json::json!({
            "mcpServers": {
                "nucleos": {"type": "stdio", "command": "nucleos-core", "args": ["--mcp-tools"]},
                "gh": {
                    "type": "stdio",
                    "command": "gh-mcp",
                    "args": ["serve"],
                    "env": {"GH_TOKEN": "s3cret", "GH_HOST": "example"}
                }
            }
        })
    }

    #[test]
    fn codex_overrides_use_each_servers_own_env_names_never_values() {
        let config = config_with_a_registry_server();
        let request_env = vec!["NUCLEOS_RUN_ID".to_string()];
        let overrides = codex_mcp_overrides(&config, &request_env).unwrap();
        assert!(
            overrides.contains(&r#"mcp_servers.gh.env_vars=["GH_HOST","GH_TOKEN"]"#.to_string()),
            "the registry server lists its own env names"
        );
        assert!(
            overrides.contains(&r#"mcp_servers.nucleos.env_vars=["NUCLEOS_RUN_ID"]"#.to_string()),
            "nucleos keeps the request env names"
        );
        assert!(
            overrides
                .contains(&r#"mcp_servers.gh.default_tools_approval_mode="approve""#.to_string()),
            "registry servers are pre-approved"
        );
        assert!(
            overrides
                .iter()
                .all(|arg| !arg.contains("s3cret") && !arg.contains("example")),
            "no override carries an env value"
        );
    }

    #[test]
    fn codex_overrides_omit_env_vars_for_a_server_with_an_empty_env() {
        let config = serde_json::json!({
            "mcpServers": {"bare": {"type": "stdio", "command": "bare", "env": {}}}
        });
        let overrides = codex_mcp_overrides(&config, &["NUCLEOS_RUN_ID".to_string()]).unwrap();
        assert!(
            overrides
                .iter()
                .all(|arg| !arg.starts_with("mcp_servers.bare.env_vars")),
            "an empty env object lists no env names"
        );
    }

    #[test]
    fn codex_mcp_env_collects_values_and_refuses_a_collision() {
        let config = config_with_a_registry_server();
        let env = codex_mcp_env(&config, &["NUCLEOS_RUN_ID".to_string()]).unwrap();
        assert_eq!(
            env,
            vec![
                ("GH_HOST".to_string(), "example".to_string()),
                ("GH_TOKEN".to_string(), "s3cret".to_string()),
            ]
        );

        let error = codex_mcp_env(&config, &["GH_TOKEN".to_string()]).unwrap_err();
        assert!(error.contains("GH_TOKEN"), "the error names the variable");
        assert!(error.contains("gh"), "the error names the server");
        assert!(
            !error.contains("s3cret"),
            "the error never carries the value"
        );
    }

    #[test]
    fn codex_mcp_env_refuses_a_denylisted_name_in_any_case() {
        for name in [
            "PATH",
            "Path",
            "codex_home",
            "OPENAI_API_KEY",
            "SystemRoot",
            "ComSpec",
        ] {
            let config = serde_json::json!({
                "mcpServers": {"gh": {"type": "stdio", "command": "gh", "env": {name: "s3cret"}}}
            });
            let error = codex_mcp_env(&config, &[]).unwrap_err();
            assert!(
                error.contains(name),
                "{name}: the error names the variable: {error}"
            );
            assert!(
                error.contains("gh"),
                "{name}: the error names the server: {error}"
            );
            assert!(
                !error.contains("s3cret"),
                "{name}: the error never carries the value"
            );
        }
    }

    #[test]
    fn codex_mcp_env_collision_case_rule_follows_the_flag() {
        let config = serde_json::json!({
            "mcpServers": {"gh": {"type": "stdio", "command": "gh", "env": {"Gh_Token": "s3cret"}}}
        });
        let reserved = ["GH_TOKEN".to_string()];
        let error = codex_mcp_env_with(&config, &reserved, true).unwrap_err();
        assert!(error.contains("Gh_Token"), "{error}");
        assert!(
            !error.contains("s3cret"),
            "the error never carries the value"
        );
        let env = codex_mcp_env_with(&config, &reserved, false).unwrap();
        assert_eq!(env, vec![("Gh_Token".to_string(), "s3cret".to_string())]);
    }

    #[cfg(windows)]
    #[test]
    fn codex_mcp_env_collision_is_case_insensitive_on_windows() {
        let config = serde_json::json!({
            "mcpServers": {"gh": {"type": "stdio", "command": "gh", "env": {"gh_token": "x"}}}
        });
        assert!(codex_mcp_env(&config, &["GH_TOKEN".to_string()]).is_err());
    }

    #[test]
    fn codex_mcp_env_is_empty_for_the_nucleos_only_config() {
        let config = serde_json::json!({
            "mcpServers": {"nucleos": {"type": "stdio", "command": "nucleos-core", "args": []}}
        });
        assert!(codex_mcp_env(&config, &[]).unwrap().is_empty());
    }
}
