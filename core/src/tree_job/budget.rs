//! The daemon-side branch budget: the admission book's slots and the measured cost per layer.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::admission::AdmissionBook;
    use crate::tree_job::model::Manifest;
    use serde_json::json;
    use std::time::Duration;

    const LAYER: &str = "backend";
    const FLOOR_GB: f64 = 15.0;
    const PLENTY_GB: f64 = 100.0;

    fn manifest() -> Manifest {
        Manifest::new("tree-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[])
    }

    fn free_book(capacity: u32) -> AdmissionBook {
        AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        )
    }

    /// A book with `held` units granted, one per run.
    fn book_holding(capacity: u32, held: u32) -> AdmissionBook {
        let book = free_book(capacity);
        for run_id in 0..held {
            book.request(i64::from(run_id) + 1, "gate", 1);
        }
        assert_eq!(book.free(), capacity - held);
        book
    }

    fn full_book(capacity: u32) -> AdmissionBook {
        book_holding(capacity, capacity)
    }

    fn names_slots(admission: &Admission) -> bool {
        let names = |reason: &String| reason.to_lowercase().contains("slot");
        admission.reasons.iter().any(names)
    }

    /// `tree_resources.py` writes `disco:`; the daemon may say either.
    fn names_disk(admission: &Admission) -> bool {
        let names = |reason: &String| {
            let reason = reason.to_lowercase();
            reason.contains("disk") || reason.contains("disco")
        };
        admission.reasons.iter().any(names)
    }

    #[test]
    fn the_seed_is_eight_gb() {
        assert_eq!(SEED_COST_GB, 8.0);
    }

    #[test]
    fn a_docs_layer_costs_nothing_whatever_costs_gb_says() {
        let mut manifest = manifest();
        assert_eq!(layer_cost_gb(&manifest, "docs"), 0.0);

        manifest.costs_gb.insert("docs".to_owned(), json!(5));
        assert_eq!(layer_cost_gb(&manifest, "docs"), 0.0);
    }

    #[test]
    fn an_unmeasured_layer_costs_the_seed() {
        let mut manifest = manifest();
        manifest.costs_gb.insert("frontend".to_owned(), json!(2.5));

        assert_eq!(layer_cost_gb(&manifest, LAYER), SEED_COST_GB);
    }

    #[test]
    fn a_layer_measured_in_a_whole_number_costs_the_measure() {
        let mut manifest = manifest();
        // 3 tells the measure from the seed; 8 is the form `tree.py` may write.
        manifest.costs_gb.insert("small".to_owned(), json!(3));
        manifest.costs_gb.insert("large".to_owned(), json!(8));

        assert_eq!(layer_cost_gb(&manifest, "small"), 3.0);
        assert_eq!(layer_cost_gb(&manifest, "large"), 8.0);
    }

    #[test]
    fn a_layer_measured_in_a_fraction_costs_the_measure() {
        let mut manifest = manifest();
        manifest.costs_gb.insert(LAYER.to_owned(), json!(2.5));

        assert_eq!(layer_cost_gb(&manifest, LAYER), 2.5);
    }

    #[test]
    fn the_first_positive_measure_is_recorded_and_returned() {
        let mut manifest = manifest();

        assert_eq!(record_measured_cost(&mut manifest, LAYER, 3.25), Some(3.25));

        assert_eq!(manifest.costs_gb[LAYER].as_f64(), Some(3.25));
        assert_eq!(layer_cost_gb(&manifest, LAYER), 3.25);
    }

    #[test]
    fn a_second_measure_does_not_overwrite_the_first() {
        let mut manifest = manifest();
        record_measured_cost(&mut manifest, LAYER, 3.25);

        assert_eq!(record_measured_cost(&mut manifest, LAYER, 6.5), None);

        assert_eq!(layer_cost_gb(&manifest, LAYER), 3.25);
    }

    #[test]
    fn a_measure_of_zero_records_nothing_and_leaves_the_first_measure_open() {
        let mut manifest = manifest();

        assert_eq!(record_measured_cost(&mut manifest, LAYER, 0.0), None);

        assert!(manifest.costs_gb.is_empty());
        assert_eq!(layer_cost_gb(&manifest, LAYER), SEED_COST_GB);
        assert_eq!(record_measured_cost(&mut manifest, LAYER, 1.5), Some(1.5));
    }

    #[test]
    fn a_docs_measure_records_nothing() {
        let mut manifest = manifest();

        assert_eq!(record_measured_cost(&mut manifest, "docs", 3.25), None);

        assert!(manifest.costs_gb.is_empty());
    }

    #[test]
    fn a_free_book_and_enough_disk_admits_at_the_layer_cost() {
        let book = free_book(2);

        let admission = check_admission(&book, &manifest(), LAYER, PLENTY_GB, FLOOR_GB);

        let expected = Admission {
            admit: true,
            cost_gb: SEED_COST_GB,
            reasons: Vec::new(),
        };
        assert_eq!(admission, expected);
    }

    #[test]
    fn a_full_book_refuses_with_a_reason_naming_slots() {
        let book = full_book(2);

        let admission = check_admission(&book, &manifest(), LAYER, PLENTY_GB, FLOOR_GB);

        assert!(!admission.admit);
        assert_eq!(admission.cost_gb, SEED_COST_GB);
        assert!(names_slots(&admission));
        assert!(!names_disk(&admission));
    }

    #[test]
    fn disk_that_leaves_exactly_the_floor_after_the_cost_admits() {
        let book = free_book(2);

        // 23 - 8 = 15: the python `int(remaining // cost) < 1` refuses only below the cost.
        let admission = check_admission(&book, &manifest(), LAYER, 23.0, FLOOR_GB);

        assert!(admission.admit);
        assert!(admission.reasons.is_empty());
    }

    #[test]
    fn disk_that_would_leave_less_than_the_floor_after_the_cost_refuses() {
        let book = free_book(2);

        // Above the floor itself, but 22.5 - 8 = 14.5 is below it.
        let admission = check_admission(&book, &manifest(), LAYER, 22.5, FLOOR_GB);

        assert!(!admission.admit);
        assert_eq!(admission.cost_gb, SEED_COST_GB);
        assert!(names_disk(&admission));
        assert!(!names_slots(&admission));
    }

    #[test]
    fn a_smaller_measure_admits_what_the_seed_refuses() {
        let book = free_book(2);
        let mut manifest = manifest();

        let at_the_seed = check_admission(&book, &manifest, LAYER, 20.0, FLOOR_GB);
        assert!(!at_the_seed.admit);
        assert_eq!(at_the_seed.cost_gb, SEED_COST_GB);

        record_measured_cost(&mut manifest, LAYER, 2.5);

        let at_the_measure = check_admission(&book, &manifest, LAYER, 20.0, FLOOR_GB);
        assert!(at_the_measure.admit);
        assert_eq!(at_the_measure.cost_gb, 2.5);
    }

    #[test]
    fn docs_admit_with_a_full_book_and_disk_below_the_floor() {
        let book = full_book(2);

        let admission = check_admission(&book, &manifest(), "docs", 1.0, FLOOR_GB);

        let expected = Admission {
            admit: true,
            cost_gb: 0.0,
            reasons: Vec::new(),
        };
        assert_eq!(admission, expected);
    }

    #[test]
    fn a_refusal_for_both_slots_and_disk_lists_both_reasons() {
        let book = full_book(2);

        let admission = check_admission(&book, &manifest(), LAYER, 10.0, FLOOR_GB);

        assert!(!admission.admit);
        assert!(names_slots(&admission));
        assert!(names_disk(&admission));
    }

    #[test]
    fn checking_admission_takes_no_slot() {
        let book = book_holding(3, 1);

        let admission = check_admission(&book, &manifest(), LAYER, PLENTY_GB, FLOOR_GB);

        assert!(admission.admit);
        assert_eq!(book.free(), 2);
        assert_eq!(book.snapshot().used, 1);
    }
}
