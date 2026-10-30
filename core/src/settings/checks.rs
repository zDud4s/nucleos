//! Validators lifted out of the YAML parsers. `parse_verify_config` and `parse_pins` call these, and so does the
//! document registry, so a body written through the API and a file read at startup meet the same rules.

use crate::config::VerifyConfig;
use crate::workflows::Pins;

impl VerifyConfig {
    /// Every number the broker divides by or waits on must be at least 1.
    #[allow(dead_code)] // GREEN wires the parsers and the registry to this.
    pub(crate) fn check(&self) -> Result<(), String> {
        todo!("RED stub: VerifyConfig::check")
    }
}

impl Pins {
    /// A name and a version are path components, so each must pass `valid_name`; a workflow is pinned once.
    #[allow(dead_code)] // GREEN wires the parsers and the registry to this.
    pub(crate) fn check(&self) -> Result<(), String> {
        todo!("RED stub: Pins::check")
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use crate::config::{
        AutopilotRules, BrowserConfig, CalendarConfig, DevtimeConfig, EmailConfig, GithubConfig, ModelsConfig,
        VerifyConfig, VoiceConfig, WebConfig,
    };
    use crate::workflows::Pins;

    fn round_trip<T: Serialize + DeserializeOwned + Default>() {
        let before = serde_json::to_value(T::default()).expect("serialises");
        let back: T = serde_json::from_value(before.clone()).expect("reads back");
        assert_eq!(
            serde_json::to_value(back).expect("serialises again"),
            before,
            "{} must survive a JSON round trip",
            std::any::type_name::<T>()
        );
    }

    #[test]
    fn every_defaultable_document_round_trips_through_json() {
        // BrowserConfig has no PartialEq, hence the comparison on JSON.
        round_trip::<ModelsConfig>();
        round_trip::<EmailConfig>();
        round_trip::<CalendarConfig>();
        round_trip::<VerifyConfig>();
        round_trip::<WebConfig>();
        round_trip::<BrowserConfig>();
        round_trip::<GithubConfig>();
        round_trip::<VoiceConfig>();
        round_trip::<DevtimeConfig>();
        round_trip::<AutopilotRules>();
        round_trip::<Pins>();
    }

    #[test]
    fn verify_check_accepts_the_defaults() {
        assert_eq!(VerifyConfig::default().check(), Ok(()));
    }

    #[test]
    fn verify_check_refuses_each_zero() {
        let cases: [(VerifyConfig, &str); 5] = [
            (VerifyConfig { capacity: 0, ..VerifyConfig::default() }, "capacity must be at least 1"),
            (VerifyConfig { aging_seconds: 0, ..VerifyConfig::default() }, "aging_seconds must be at least 1"),
            (
                VerifyConfig { unit_timeout_seconds: 0, ..VerifyConfig::default() },
                "unit_timeout_seconds must be at least 1",
            ),
            (VerifyConfig { disk_cap_gb: 0, ..VerifyConfig::default() }, "disk_cap_gb must be at least 1"),
            (VerifyConfig { cache_days: 0, ..VerifyConfig::default() }, "cache_days must be at least 1"),
        ];
        for (config, message) in cases {
            assert_eq!(config.check(), Err(message.to_owned()));
        }
    }

    fn pins(yaml: &str) -> Pins {
        serde_yaml::from_str(yaml).expect("pins")
    }

    #[test]
    fn pins_check_accepts_distinct_valid_names() {
        let pins = pins(
            "workflows:\n  - {name: a, version: v1, origin: o, hash: h}\n  - {name: b, version: v1, origin: o, hash: h}\n",
        );
        assert_eq!(pins.check(), Ok(()));
        assert_eq!(Pins::default().check(), Ok(()));
    }

    #[test]
    fn pins_check_refuses_a_name_that_is_a_path() {
        let pins = pins("workflows:\n  - {name: '../etc', version: v1, origin: o, hash: h}\n");
        assert_eq!(pins.check(), Err("`../etc` is not a usable workflow name".to_owned()));
    }

    #[test]
    fn pins_check_refuses_a_version_that_is_a_path() {
        let pins = pins("workflows:\n  - {name: a, version: '../x', origin: o, hash: h}\n");
        assert_eq!(pins.check(), Err("`../x` is not a usable version for `a`".to_owned()));
    }

    #[test]
    fn pins_check_refuses_one_workflow_pinned_twice() {
        let pins = pins(
            "workflows:\n  - {name: a, version: v1, origin: o, hash: h}\n  - {name: a, version: v2, origin: o, hash: h}\n",
        );
        assert_eq!(pins.check(), Err("`a` is pinned twice".to_owned()));
    }
}
