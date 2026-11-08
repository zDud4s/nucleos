//! §spec workflow-auto-afinacao
//!
//! The tunable parameters a workflow bundle declares, and the bound checking for a value.
//!
//! A bundle's `bundle.yaml` may carry a `params:` list: a name, a type, bounds, a tier, and the
//! YAML file and key (inside the project's state directory) where the value lives. The daemon knows
//! only this contract, never the meaning of a key — no parameter of any bundle is named in the core.
//!
//! The manifest is read from the LIBRARY copy of the pinned version, never from an ejected copy in
//! the working tree: an agent that can edit the copy must not be able to widen a bound.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflows;

/// The type of a declared parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    Int,
    Float,
    Bool,
    Enum,
}

/// One declared parameter, as a bundle's manifest describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    /// Unique in the bundle; `[A-Za-z0-9_.-]+`, at most 64 characters.
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// 1 = the tuner may write it; 2 or 3 = owner only.
    pub tier: u8,
    /// Relative to the state directory, forward slashes, `.yaml` or `.yml`.
    pub file: String,
    /// Dotted path inside the file, one or two segments.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Why a value was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub reason: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

fn violation(reason: impl Into<String>) -> Violation {
    Violation {
        reason: reason.into(),
    }
}

impl ParamDecl {
    /// Check `value` against the declared type and bounds, returning the normalised value.
    /// An int-typed `4.0` or `true` is refused, not coerced.
    pub fn check(&self, value: &Value) -> Result<Value, Violation> {
        match self.ty {
            ParamType::Bool => match value {
                Value::Bool(_) => Ok(value.clone()),
                _ => Err(violation(format!("{} must be a boolean", self.name))),
            },
            ParamType::Enum => match value.as_str() {
                Some(text) if self.values.iter().any(|v| v == text) => Ok(value.clone()),
                _ => Err(violation(format!(
                    "{} must be one of {}",
                    self.name,
                    self.values.join(", ")
                ))),
            },
            ParamType::Int => {
                let Some(number) = value.as_i64() else {
                    return Err(violation(format!("{} must be an integer", self.name)));
                };
                self.in_range(number as f64)?;
                Ok(Value::from(number))
            }
            ParamType::Float => {
                let Some(number) = value.as_f64().filter(|n| n.is_finite()) else {
                    return Err(violation(format!("{} must be a finite number", self.name)));
                };
                self.in_range(number)?;
                Ok(value.clone())
            }
        }
    }

    fn in_range(&self, number: f64) -> Result<(), Violation> {
        if self.min.is_some_and(|min| number < min) || self.max.is_some_and(|max| number > max) {
            return Err(violation(format!(
                "{} must be within {}..={}",
                self.name,
                self.min.map_or("-inf".to_string(), |v| v.to_string()),
                self.max.map_or("inf".to_string(), |v| v.to_string()),
            )));
        }
        Ok(())
    }
}

/// Whether `file` is a safe path under the state directory: relative, forward slashes, no `..`,
/// no drive letter, no empty segment, and a `.yaml` or `.yml` name.
pub fn safe_relative(file: &str) -> bool {
    if file.is_empty() || file.contains('\\') || file.starts_with('/') || file.contains(':') {
        return false;
    }
    if !(file.ends_with(".yaml") || file.ends_with(".yml")) {
        return false;
    }
    file.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// The manifest's `params:` value as declarations. Absent and `null` are no params.
pub fn parse(raw: serde_yaml::Value) -> Result<Vec<ParamDecl>, String> {
    if raw.is_null() {
        return Ok(Vec::new());
    }
    serde_yaml::from_value(raw).map_err(|error| format!("params: {error}"))
}

/// Check the declarations against each other and against the contract.
pub fn validate(decls: &[ParamDecl]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for decl in decls {
        let name = &decl.name;
        if !valid_name(name) {
            return Err(format!("param `{name}`: not a usable name"));
        }
        if !seen.insert(name.as_str()) {
            return Err(format!("param `{name}` is declared twice"));
        }
        if !(1..=3).contains(&decl.tier) {
            return Err(format!("param `{name}`: tier must be 1, 2 or 3"));
        }
        if !safe_relative(&decl.file) {
            return Err(format!(
                "param `{name}`: file `{}` must be a relative .yaml/.yml path inside the state directory",
                decl.file
            ));
        }
        let segments: Vec<&str> = decl.key.split('.').collect();
        if segments.len() > 2 || segments.iter().any(|s| s.is_empty()) {
            return Err(format!(
                "param `{name}`: key `{}` must have one or two non-empty segments",
                decl.key
            ));
        }
        match decl.ty {
            ParamType::Int | ParamType::Float => {
                if !decl.values.is_empty() {
                    return Err(format!("param `{name}`: values is for enum only"));
                }
                for bound in [decl.min, decl.max].into_iter().flatten() {
                    if !bound.is_finite() {
                        return Err(format!("param `{name}`: bounds must be finite"));
                    }
                }
                if let (Some(min), Some(max)) = (decl.min, decl.max)
                    && min > max
                {
                    return Err(format!("param `{name}`: min is above max"));
                }
            }
            ParamType::Bool | ParamType::Enum => {
                if decl.min.is_some() || decl.max.is_some() {
                    return Err(format!("param `{name}`: min/max are for int and float only"));
                }
                if decl.ty == ParamType::Bool && !decl.values.is_empty() {
                    return Err(format!("param `{name}`: values is for enum only"));
                }
                if decl.ty == ParamType::Enum {
                    let unique: HashSet<&String> = decl.values.iter().collect();
                    if decl.values.is_empty() || unique.len() != decl.values.len() {
                        return Err(format!(
                            "param `{name}`: an enum needs non-empty, unique values"
                        ));
                    }
                }
            }
        }
        if let Some(default) = &decl.default {
            decl.check(default)
                .map_err(|v| format!("param `{name}`: default: {v}"))?;
        }
    }
    Ok(())
}

/// The declarations of the bundle in `dir` (its `bundle.yaml`), parsed and validated.
pub fn manifest_params(dir: &Path) -> Result<Vec<ParamDecl>, String> {
    let manifest_path = dir.join(workflows::MANIFEST);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest: workflows::ManifestFile = serde_yaml::from_str::<Option<_>>(&text)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?
        .unwrap_or_default();
    parse(manifest.params)
        .and_then(|decls| validate(&decls).map(|()| decls))
        .map_err(|error| format!("{}: {error}", manifest_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn int_decl() -> ParamDecl {
        ParamDecl {
            name: "a.b".into(),
            ty: ParamType::Int,
            min: Some(1.0),
            max: Some(12.0),
            values: vec![],
            default: None,
            tier: 1,
            file: "project.yaml".into(),
            key: "a.b".into(),
            description: None,
        }
    }

    fn parse_yaml(text: &str) -> Result<Vec<ParamDecl>, String> {
        parse(serde_yaml::from_str(text).unwrap())
    }

    #[test]
    fn a_null_params_value_is_no_params() {
        assert_eq!(parse(serde_yaml::Value::Null).unwrap(), vec![]);
    }

    #[test]
    fn int_bounds_are_inclusive_and_a_float_or_bool_is_not_an_int() {
        let d = int_decl();
        assert_eq!(d.check(&json!(1)).unwrap(), json!(1));
        assert!(d.check(&json!(12)).is_ok());
        assert!(d.check(&json!(0)).is_err());
        assert!(d.check(&json!(13)).is_err());
        assert!(d.check(&json!(4.5)).is_err());
        assert!(d.check(&json!(4.0)).is_err());
        assert!(d.check(&json!(true)).is_err());
        assert!(d.check(&json!("4")).is_err());
    }

    #[test]
    fn float_bounds_reject_nan_and_infinity() {
        let mut d = int_decl();
        d.ty = ParamType::Float;
        d.min = Some(0.0);
        d.max = Some(1.0);
        assert!(d.check(&json!(0.5)).is_ok());
        assert!(d.check(&json!(1.5)).is_err());
        assert!(d.check(&json!("NaN")).is_err());
        let huge: Value = serde_json::from_str("1e999").unwrap_or(json!("inf"));
        assert!(d.check(&huge).is_err());
    }

    #[test]
    fn enum_accepts_only_declared_values() {
        let mut d = int_decl();
        d.ty = ParamType::Enum;
        d.min = None;
        d.max = None;
        d.values = vec!["a".into(), "b".into()];
        assert!(d.check(&json!("a")).is_ok());
        assert!(d.check(&json!("c")).is_err());
        assert!(d.check(&json!(1)).is_err());
    }

    #[test]
    fn bool_accepts_only_booleans() {
        let mut d = int_decl();
        d.ty = ParamType::Bool;
        d.min = None;
        d.max = None;
        assert!(d.check(&json!(false)).is_ok());
        assert!(d.check(&json!(0)).is_err());
        assert!(d.check(&json!("true")).is_err());
    }

    #[test]
    fn validate_refuses_duplicate_names_unknown_tier_min_above_max_and_a_default_that_fails_check() {
        assert!(validate(&[int_decl()]).is_ok());
        assert!(validate(&[int_decl(), int_decl()]).is_err());
        let mut d = int_decl();
        d.tier = 0;
        assert!(validate(&[d]).is_err());
        let mut d = int_decl();
        d.tier = 4;
        assert!(validate(&[d]).is_err());
        let mut d = int_decl();
        d.min = Some(20.0);
        assert!(validate(&[d]).is_err());
        let mut d = int_decl();
        d.default = Some(json!(99));
        assert!(validate(&[d]).is_err());
        let mut d = int_decl();
        d.default = Some(json!(4));
        assert!(validate(&[d]).is_ok());
    }

    #[test]
    fn validate_refuses_empty_enum_values_and_bounds_on_a_bool() {
        let mut d = int_decl();
        d.ty = ParamType::Enum;
        d.min = None;
        d.max = None;
        assert!(validate(&[d.clone()]).is_err());
        d.values = vec!["a".into(), "a".into()];
        assert!(validate(&[d.clone()]).is_err());
        d.values = vec!["a".into()];
        assert!(validate(&[d]).is_ok());
        let mut b = int_decl();
        b.ty = ParamType::Bool;
        assert!(validate(&[b]).is_err());
    }

    #[test]
    fn validate_refuses_a_file_outside_the_state_directory() {
        for file in [
            "../x.yaml",
            "/etc/x.yaml",
            "C:/x.yaml",
            "C:\\x.yaml",
            "a\\b.yaml",
            "",
            "sub/../../x.yaml",
            "x.txt",
            "//host/share/x.yaml",
        ] {
            let mut d = int_decl();
            d.file = file.into();
            assert!(validate(&[d]).is_err(), "{file:?} must be refused");
        }
        let mut d = int_decl();
        d.file = "sub/x.yml".into();
        assert!(validate(&[d]).is_ok());
    }

    #[test]
    fn validate_refuses_a_key_with_more_than_two_segments_or_an_empty_segment() {
        for key in ["a.b.c", "", "a.", ".a", "a..b"] {
            let mut d = int_decl();
            d.key = key.into();
            assert!(validate(&[d]).is_err(), "{key:?} must be refused");
        }
        let mut d = int_decl();
        d.key = "solo".into();
        assert!(validate(&[d]).is_ok());
    }

    #[test]
    fn an_unknown_field_in_a_param_is_refused() {
        let text = "- name: a\n  tyep: int\n  tier: 1\n  file: p.yaml\n  key: a\n";
        assert!(parse_yaml(text).is_err());
        let ok = "- name: a\n  type: int\n  min: 1\n  max: 2\n  tier: 1\n  file: p.yaml\n  key: a\n";
        assert_eq!(parse_yaml(ok).unwrap().len(), 1);
    }

    #[test]
    fn manifest_params_reads_a_bundle_directory_and_a_missing_manifest_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(manifest_params(dir.path()).is_err());
        std::fs::write(
            dir.path().join(workflows::MANIFEST),
            "params:\n  - name: a\n    type: int\n    min: 1\n    max: 5\n    tier: 1\n    file: p.yaml\n    key: a\n",
        )
        .unwrap();
        assert_eq!(manifest_params(dir.path()).unwrap().len(), 1);
        std::fs::write(
            dir.path().join(workflows::MANIFEST),
            "params:\n  - name: a\n    type: int\n    tier: 1\n    file: ../p.yaml\n    key: a\n",
        )
        .unwrap();
        assert!(manifest_params(dir.path()).is_err());
    }

    #[test]
    fn the_nucleos_dev_asset_declares_valid_params() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/nucleos-dev");
        let decls = manifest_params(&dir).unwrap();
        let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
        for expected in [
            "tree.max_live_branches",
            "tree.floor_gb",
            "parallel_execute.max_workers",
            "parallel_execute.max_units",
            "parallel_execute.max_wave_seconds",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert_eq!(decls.len(), 5);
        for d in &decls {
            assert!(d.file == "project.yaml" || d.file == "models.yaml");
            if let Some(default) = &d.default {
                assert!(d.check(default).is_ok());
            }
        }
        let by = |n: &str| decls.iter().find(|d| d.name == n).unwrap();
        let b = by("tree.max_live_branches");
        assert!(b.check(&json!(6)).is_ok());
        assert!(b.check(&json!(13)).is_err());
        assert!(b.check(&json!(0)).is_err());
        assert!(by("parallel_execute.max_workers").check(&json!(2)).is_ok());
        assert!(by("parallel_execute.max_units").check(&json!(4)).is_ok());
        assert!(by("parallel_execute.max_units").check(&json!(5)).is_ok());
        assert!(by("parallel_execute.max_wave_seconds")
            .check(&json!(2400))
            .is_ok());
        assert!(by("tree.floor_gb").check(&json!(5)).is_ok());
        assert!(by("tree.floor_gb").check(&json!(4)).is_err());
        assert!(by("parallel_execute.max_wave_seconds")
            .check(&json!(5))
            .is_err());
    }
}
