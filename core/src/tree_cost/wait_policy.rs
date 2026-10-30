//! stub: wait policy decision

#[cfg(test)]
mod tests {
    use super::*;

    /// Below the default rotation threshold, so only the keep-warm rule can decide.
    const SMALL_CONTEXT: i64 = 10_000;
    /// Above the default rotation threshold of 60 000.
    const BIG_CONTEXT: i64 = 100_000;
    const ONE_HOUR_S: i64 = 3_600;

    fn input(
        context_tokens: i64,
        expected_wait_s: i64,
        ttl_s: i64,
        resume_probability: Option<f64>,
        write_multiplier: f64,
    ) -> WaitInput {
        WaitInput {
            context_tokens,
            expected_wait_s,
            ttl_s,
            resume_probability,
            write_multiplier,
            needs_live_session: true,
        }
    }

    fn keep_warm_on() -> WaitSettings {
        WaitSettings {
            keep_warm_enabled: true,
            ..WaitSettings::default()
        }
    }

    #[test]
    fn keep_warm_one_hour_ttl_three_hours_needs_p_above_a_tenth() {
        // n = ceil(10800 / 3240) - 1 = 3, so keep-warm needs 0.3 < 1.9 * P, P above about 0.158.
        // The name comes from the plan and the spec's order-of-magnitude example; the formula is
        // normative. The margins are deliberate: 0.05 and 0.20 stay on the same side of the
        // threshold for any plausible rounding, so this pins the rule and not its float noise.
        let set = keep_warm_on();
        let wait = |p| input(SMALL_CONTEXT, 10_800, ONE_HOUR_S, Some(p), 2.0);

        assert_eq!(decide_wait(&wait(0.05), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.20), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn keep_warm_one_hour_ttl_twelve_hours_needs_p_above_half() {
        // n = ceil(43200 / 3240) - 1 = 13, so keep-warm needs 1.3 < 1.9 * P, P above about 0.684.
        let set = keep_warm_on();
        let wait = |p| input(SMALL_CONTEXT, 43_200, ONE_HOUR_S, Some(p), 2.0);

        assert_eq!(decide_wait(&wait(0.40), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.75), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn five_minute_ttl_never_keeps_warm_for_an_hour() {
        // n = ceil(3600 / 270) - 1 = 13 and w - r = 1.15: 1.3 is not below 1.15 even at P = 1.
        let inp = input(SMALL_CONTEXT, ONE_HOUR_S, 300, Some(1.0), 1.25);

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Park);
    }

    #[test]
    fn rotate_wins_for_large_context_when_wait_exceeds_ttl() {
        // n = ceil(7200 / 3240) - 1 = 2, and 0.2 < 1.9: keep-warm would also pass, Rotate goes first.
        let inp = input(BIG_CONTEXT, 7_200, ONE_HOUR_S, Some(1.0), 2.0);

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Rotate);
    }

    #[test]
    fn park_is_default_when_no_live_session_needed() {
        // The same big context and long wait that rotate in the test above.
        let inp = WaitInput {
            needs_live_session: false,
            ..input(BIG_CONTEXT, 7_200, ONE_HOUR_S, Some(1.0), 2.0)
        };

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Park);
    }

    #[test]
    fn keep_warm_disabled_in_default_settings() {
        let set = WaitSettings::default();
        assert_eq!(set.rotate_context_tokens, 60_000);
        assert_eq!(set.rotate_min_wait_over_ttl, 1.0);
        assert!(!set.keep_warm_enabled);
        assert_eq!(set.resume_probability, 0.5);
        assert_eq!(set.read_multiplier, 0.1);

        // n = ceil(3600 / 3240) - 1 = 1, and 0.1 < 1.9: only the disabled flag keeps this parked,
        // which the enabled control below proves.
        let inp = input(SMALL_CONTEXT, ONE_HOUR_S, ONE_HOUR_S, Some(1.0), 2.0);
        assert_eq!(decide_wait(&inp, &set), WaitAction::Park);
        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::KeepWarm);
    }

    #[test]
    fn boundary_equality_does_not_keep_warm() {
        // 0.25 and 1.25 are exact in binary, so with n = 1 both sides of the comparison are exact:
        // n * r = 0.25 and w - r = 1.0.
        let set = WaitSettings {
            read_multiplier: 0.25,
            ..keep_warm_on()
        };
        let wait = |p| input(SMALL_CONTEXT, ONE_HOUR_S, ONE_HOUR_S, Some(p), 1.25);

        assert_eq!(decide_wait(&wait(0.25), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.26), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn rotate_needs_strictly_larger_context_and_wait() {
        // Default settings keep keep-warm off, so whatever does not rotate parks.
        let set = WaitSettings::default();
        let threshold = set.rotate_context_tokens;
        let decide = |context, wait_s| {
            decide_wait(
                &input(context, wait_s, ONE_HOUR_S, Some(1.0), 2.0),
                &set,
            )
        };

        // Context exactly at the threshold, with a wait well over the TTL.
        assert_eq!(decide(threshold, 2 * ONE_HOUR_S), WaitAction::Park);
        // Wait exactly at the TTL, with a context well over the threshold.
        assert_eq!(decide(BIG_CONTEXT, ONE_HOUR_S), WaitAction::Park);
        // One past both limits rotates, so the two cases above fail for the reason they state.
        assert_eq!(decide(threshold + 1, ONE_HOUR_S + 1), WaitAction::Rotate);
    }

    #[test]
    fn input_probability_overrides_the_settings_default() {
        // n = 3, so the threshold is about 0.158: 0.05 parks and 0.20 keeps warm.
        let wait = |p| input(SMALL_CONTEXT, 10_800, ONE_HOUR_S, p, 2.0);
        let settings_with = |p| WaitSettings {
            resume_probability: p,
            ..keep_warm_on()
        };

        // Without an input probability, the settings one decides.
        assert_eq!(decide_wait(&wait(None), &settings_with(0.05)), WaitAction::Park);
        assert_eq!(decide_wait(&wait(None), &settings_with(0.20)), WaitAction::KeepWarm);
        // With one, it beats the settings in both directions.
        assert_eq!(decide_wait(&wait(Some(0.20)), &settings_with(0.05)), WaitAction::KeepWarm);
        assert_eq!(decide_wait(&wait(Some(0.05)), &settings_with(0.20)), WaitAction::Park);
    }
}
