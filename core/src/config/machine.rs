//! The daemon role and display name stored with the machine settings.

/// Whether this daemon owns machine-wide channel and routine supervision.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
    PartialEq,
    Eq,
)]
#[serde(rename_all = "lowercase")]
pub enum MachineRole {
    #[default]
    Primary,
    Secondary,
}

/// A machine-scoped identity and role.
///
/// An absent name is resolved when it is read so an older document needs no migration and keeps
/// identifying this machine by its system hostname.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MachineConfig {
    pub name: Option<String>,
    pub role: MachineRole,
}

impl MachineConfig {
    /// The configured name, or the hostname for a machine that has never named itself.
    pub fn name(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(system_hostname)
    }
}

fn system_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "NucleOS".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_with_no_role_stays_primary() {
        let machine: MachineConfig = serde_json::from_value(serde_json::json!({})).unwrap();

        assert_eq!(machine.role, MachineRole::Primary);
    }
}
