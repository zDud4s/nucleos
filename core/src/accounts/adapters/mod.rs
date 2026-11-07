//! The provider adapters, and the production registry that finds one by engine.

pub mod claude;
pub mod codex;
#[cfg(any(test, feature = "testkit"))]
pub mod fake;

use super::provider::ProviderAdapter;

static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static CODEX: codex::CodexAdapter = codex::CodexAdapter;

/// The production registry. None for engines outside the pool (local, agy, gemini-api).
pub fn adapter_for(engine: &str) -> Option<&'static dyn ProviderAdapter> {
    match engine {
        claude::ENGINE => Some(&CLAUDE),
        codex::ENGINE => Some(&CODEX),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pooled_engines_have_an_adapter() {
        assert!(adapter_for("claude").is_some());
        assert!(adapter_for("codex").is_some());
        assert!(adapter_for("local").is_none());
        assert!(adapter_for("agy").is_none());
    }
}
