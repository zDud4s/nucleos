//! Engine descriptors as data (sandbox plan D3): what the sandbox needs to know about an engine
//! CLI without a branch per engine — its binary, the variable that relocates its config directory,
//! the flag that points it at a settings file, and the API domains it must reach.
//!
//! The descriptor is owner data kept in `nucleos.db` (a settings document), never a file in a
//! project tree and never under `.ai/`. A descriptor that names a path the sandbox may write (a
//! grant) or a retired `.ai/` location is refused: a unit must not be able to rewrite how its own
//! engine is launched.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::{Document, Kind, Scope};

/// How one engine CLI is launched and confined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EngineDescriptor {
    /// The engine id (`claude`, `codex`, ...).
    pub id: String,
    /// The program that runs the engine: a bare name resolved on PATH, or an absolute path.
    pub binary: String,
    /// The environment variable that relocates the engine's config directory.
    pub config_var: String,
    /// The CLI flag that names a settings file, when the engine has one.
    #[serde(default)]
    pub settings_flag: Option<String>,
    /// The API domains the engine must reach (the network allowlist of its unit).
    #[serde(default)]
    pub domains: Vec<String>,
}

/// The stored set of descriptors. Empty until the owner changes one: the built-ins apply.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EngineDescriptorsDoc {
    pub descriptors: Vec<EngineDescriptor>,
}

impl Document for EngineDescriptorsDoc {
    const NAME: &'static str = "engine_descriptors";
    const KIND: Kind = Kind::Machine;
    fn finish(self) -> Result<Self, String> {
        let mut seen = std::collections::BTreeSet::new();
        for descriptor in &self.descriptors {
            descriptor.validate()?;
            if !seen.insert(descriptor.id.as_str()) {
                return Err(format!("engine `{}` is described twice", descriptor.id));
            }
        }
        Ok(self)
    }
}

impl EngineDescriptor {
    /// Shape checks that need no grants: the same ones the document's `finish` applies.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.binary.trim().is_empty() {
            return Err("an engine descriptor needs an id and a binary".to_owned());
        }
        if self.config_var.trim().is_empty() {
            return Err(format!("engine `{}` names no config variable", self.id));
        }
        if let Some(flag) = &self.settings_flag
            && !flag.starts_with("--")
        {
            return Err(format!(
                "engine `{}`: the settings flag `{flag}` is not a long option",
                self.id
            ));
        }
        if is_retired_layout(Path::new(&self.binary)) {
            return Err(format!(
                "engine `{}`: the binary `{}` lies under `.ai/`, a retired layout",
                self.id, self.binary
            ));
        }
        Ok(())
    }

    /// Refuses a descriptor whose binary is a path under one of the sandbox `grants`: whatever the
    /// unit may write it could replace, and the engine would then run as the owner.
    pub fn refuse_under_grants(&self, grants: &[PathBuf]) -> Result<(), String> {
        self.validate()?;
        let binary = Path::new(&self.binary);
        if !binary.is_absolute() {
            return Ok(());
        }
        let real = binary
            .canonicalize()
            .unwrap_or_else(|_| binary.to_path_buf());
        for grant in grants {
            let real_grant = grant.canonicalize().unwrap_or_else(|_| grant.clone());
            if real.starts_with(&real_grant) || binary.starts_with(grant) {
                return Err(format!(
                    "engine `{}`: the binary {} is under the sandbox grant {}",
                    self.id,
                    binary.display(),
                    grant.display()
                ));
            }
        }
        Ok(())
    }
}

fn is_retired_layout(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".ai")
}

/// The descriptors this daemon ships. Claude and codex are the engines with a config variable.
pub fn builtin() -> Vec<EngineDescriptor> {
    vec![
        EngineDescriptor {
            id: crate::engines::CLAUDE.to_owned(),
            binary: "claude".to_owned(),
            config_var: "CLAUDE_CONFIG_DIR".to_owned(),
            settings_flag: Some("--settings".to_owned()),
            domains: vec!["api.anthropic.com".to_owned()],
        },
        EngineDescriptor {
            id: crate::engines::CODEX.to_owned(),
            binary: "codex".to_owned(),
            config_var: "CODEX_HOME".to_owned(),
            settings_flag: None,
            domains: vec!["api.openai.com".to_owned()],
        },
    ]
}

/// The descriptor of `id`: the stored one when the owner set it, else the built-in, else `None`.
pub fn descriptor_for(id: &str) -> Option<EngineDescriptor> {
    let stored = crate::settings::get_opt::<EngineDescriptorsDoc>(&Scope::Machine);
    stored
        .as_deref()
        .and_then(|doc| doc.descriptors.iter().find(|d| d.id == id).cloned())
        .or_else(|| builtin().into_iter().find(|d| d.id == id))
}
