//! The provider adapters, and the production registry that finds one by engine.

pub mod claude;
pub mod codex;
#[cfg(any(test, feature = "testkit"))]
pub mod fake;

use super::provider::ProviderAdapter;

static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static CODEX: codex::CodexAdapter = codex::CodexAdapter;

/// The one table both `adapter_for` and `pooled_engines` read, so the two cannot drift.
static REGISTRY: &[(&str, &dyn ProviderAdapter)] =
    &[(claude::ENGINE, &CLAUDE), (codex::ENGINE, &CODEX)];

/// The production registry. None for engines outside the pool (local, agy, gemini-api).
pub fn adapter_for(engine: &str) -> Option<&'static dyn ProviderAdapter> {
    REGISTRY
        .iter()
        .find(|(name, _)| *name == engine)
        .map(|(_, adapter)| *adapter)
}

/// The engines the pool can hold an account for, in registry order.
pub fn pooled_engines() -> Vec<&'static str> {
    REGISTRY.iter().map(|(name, _)| *name).collect()
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

    #[test]
    fn every_pooled_engine_has_an_adapter() {
        let engines = pooled_engines();
        assert_eq!(engines, vec!["claude", "codex"]);
        for engine in engines {
            assert!(adapter_for(engine).is_some(), "{engine} has no adapter");
        }
    }
}
