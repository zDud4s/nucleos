//! What the root start does with a refusal the disk alone caused.
//!
//! The root branch is a code layer, so it is charged the seed cost against the free disk like any
//! branch (`budget::check_admission`). A refusal of that kind is the one a freed disk undoes, and the
//! owner is the one who frees it: so the sweep retries it, and after `DISK_START_CAP` refusals the
//! owner is asked once, with the figure of room the start needs, instead of the generic five tries.

/// Refused starts of a root, for a disk refusal, before the owner is asked.
pub(super) const DISK_START_CAP: u64 = 2;

/// The figures in a disk refusal (`budget::check_admission`'s reason), in GB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DiskRefusal {
    pub free_gb: f64,
    pub cost_gb: f64,
    pub floor_gb: f64,
}

impl DiskRefusal {
    /// The free disk the start needs to be admitted: its cost on top of the floor.
    pub(super) fn needed_gb(&self) -> f64 {
        self.cost_gb + self.floor_gb
    }
}

/// Reads a disk refusal out of a start's error text, which holds
/// `disk: <free> GB free minus <cost> GB cost is below the <floor> GB floor`. `None` for any other
/// error.
pub(super) fn parse_disk_refusal(error: &str) -> Option<DiskRefusal> {
    let rest = &error[error.find("disk: ")? + "disk: ".len()..];
    let (free, rest) = rest.split_once(" GB free minus ")?;
    let (cost, rest) = rest.split_once(" GB cost is below the ")?;
    let (floor, _) = rest.split_once(" GB floor")?;
    Some(DiskRefusal {
        free_gb: free.trim().parse().ok()?,
        cost_gb: cost.trim().parse().ok()?,
        floor_gb: floor.trim().parse().ok()?,
    })
}

/// Failed starts allowed before the owner is asked, for this last error.
pub(super) fn start_cap(error: &str, default_cap: u64) -> u64 {
    if parse_disk_refusal(error).is_some() {
        DISK_START_CAP.min(default_cap)
    } else {
        default_cap
    }
}

/// The question put to the owner past the cap.
pub(super) fn question(job_id: &str, attempts: u64, error: &str) -> String {
    match parse_disk_refusal(error) {
        Some(refusal) => format!(
            "The root of tree job {job_id} did not start after {attempts} tries: the disk is too \
             full. It has {:.1} GB free and the start needs {:.1} GB free ({:.1} GB for the branch \
             plus the {:.1} GB floor), so free about {:.1} GB and answer to try again, or say how \
             the job should go on.",
            refusal.free_gb,
            refusal.needed_gb(),
            refusal.cost_gb,
            refusal.floor_gb,
            (refusal.needed_gb() - refusal.free_gb).max(0.0),
        ),
        None => format!(
            "The root of tree job {job_id} did not start after {attempts} tries (last error: \
             {error}). Fix the cause and answer to try again, or say how the job should go on."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSAL: &str = "the branch is not admitted: disk: 14.8 GB free minus 8.0 GB cost is \
                           below the 15.0 GB floor";

    #[test]
    fn a_disk_refusal_is_read_with_its_three_figures() {
        let refusal = parse_disk_refusal(REFUSAL).expect("a disk refusal");
        assert_eq!(
            refusal,
            DiskRefusal {
                free_gb: 14.8,
                cost_gb: 8.0,
                floor_gb: 15.0
            }
        );
        assert_eq!(refusal.needed_gb(), 23.0);
    }

    #[test]
    fn another_error_is_not_a_disk_refusal() {
        assert_eq!(parse_disk_refusal("no such repository"), None);
        assert_eq!(parse_disk_refusal("disk: lots"), None);
    }

    #[test]
    fn a_disk_refusal_asks_the_owner_sooner_than_other_errors() {
        assert_eq!(start_cap(REFUSAL, 5), 2);
        assert_eq!(start_cap("no such repository", 5), 5);
        assert_eq!(start_cap(REFUSAL, 1), 1);
    }

    #[test]
    fn the_question_for_a_disk_refusal_says_how_much_disk_is_needed() {
        let text = question("job-1", 2, REFUSAL);
        assert!(text.contains("23.0 GB free"), "{text}");
        assert!(text.contains("free about 8.2 GB"), "{text}");
        assert!(text.contains("job-1"));
    }

    #[test]
    fn the_question_for_another_error_names_the_error() {
        let text = question("job-1", 5, "no such repository");
        assert!(text.contains("last error: no such repository"), "{text}");
    }
}
