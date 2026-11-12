//! Pure detectors for the tickets the daemon opens itself (no git, no SQL).

/// Two migration files that carry the same number under different names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The four digits shared by both files.
    pub number: String,
    pub branch_file: String,
    pub target_file: String,
}

/// The `NNNN` of a migration file name (`^\d{4}_`), if it has one.
fn number_of(name: &str) -> Option<&str> {
    let prefix = name.get(..4)?;
    let underscore = name.as_bytes().get(4)?;
    (prefix.bytes().all(|b| b.is_ascii_digit()) && *underscore == b'_').then_some(prefix)
}

/// The numbers used by one file on the branch and a DIFFERENT file on the target. File NAMES, not
/// paths. The same name on both sides is the same file (the branch simply has it too), and a number
/// present on one side only is no collision.
pub fn colliding_numbers(target_files: &[String], branch_files: &[String]) -> Vec<Collision> {
    let mut found = Vec::new();
    for branch_file in branch_files {
        let Some(number) = number_of(branch_file) else {
            continue;
        };
        if target_files.contains(branch_file) {
            continue;
        }
        for target_file in target_files {
            if number_of(target_file) == Some(number) {
                found.push(Collision {
                    number: number.to_owned(),
                    branch_file: branch_file.clone(),
                    target_file: target_file.clone(),
                });
            }
        }
    }
    found.sort_by(|a, b| {
        (&a.number, &a.branch_file, &a.target_file).cmp(&(
            &b.number,
            &b.branch_file,
            &b.target_file,
        ))
    });
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn same_number_with_a_different_name_collides() {
        let found = colliding_numbers(
            &names(&["0275_a.sql", "0276_target.sql"]),
            &names(&["0275_a.sql", "0276_branch.sql"]),
        );
        assert_eq!(
            found,
            vec![Collision {
                number: "0276".to_owned(),
                branch_file: "0276_branch.sql".to_owned(),
                target_file: "0276_target.sql".to_owned(),
            }]
        );
    }

    #[test]
    fn the_same_file_on_both_sides_is_not_a_collision() {
        let both = names(&["0275_a.sql", "0276_same.sql"]);
        assert!(colliding_numbers(&both, &both).is_empty());
    }

    #[test]
    fn a_file_only_on_one_side_is_not_a_collision() {
        assert!(colliding_numbers(&names(&["0275_a.sql"]), &names(&["0276_new.sql"])).is_empty());
        assert!(colliding_numbers(&names(&["0276_new.sql"]), &names(&["0275_a.sql"])).is_empty());
    }

    #[test]
    fn a_non_numbered_file_is_ignored() {
        assert!(
            colliding_numbers(
                &names(&["README.md", "02_x.sql", "0276x.sql"]),
                &names(&["README.md", "02_y.sql", "0276y.sql"]),
            )
            .is_empty()
        );
    }
}
