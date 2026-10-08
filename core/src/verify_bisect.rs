//! Pure stepper over the first-parent merges of a red batch (spec 2026-10-05
//! §6.2 step 2). `next` asks for one sha or answers the verdict and `record`
//! takes the probe's result, so a driver can run each probe through the executor
//! and resume after a restart. Candidates are the merges in `(last_green, tip]`,
//! oldest first: the one before the first is known green and the last is known
//! red. Runs and reverts nothing; called by nothing until F3-3.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub sha: String,
    /// The merge changed the test map, so it is always worth naming.
    pub changes_map: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Green,
    Red,
    /// Like `git bisect skip`: the probe could not tell.
    Inconclusive,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    NoCandidates,
    Culprit {
        sha: String,
        also_suspect: Vec<String>,
    },
    Inconclusive {
        candidates: Vec<String>,
        also_suspect: Vec<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Probe(String),
    Done(Verdict),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RecordError {
    /// The sha is not an open candidate.
    NotAsked(String),
}

pub struct Bisection {
    candidates: Vec<Candidate>,
    /// Highest index known green; `None` means only the last green sha (index -1).
    green: Option<usize>,
    /// Lowest index known red; starts at the last candidate.
    red: usize,
    skipped: BTreeSet<usize>,
}

impl Bisection {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let red = candidates.len().saturating_sub(1);
        Self {
            candidates,
            green: None,
            red,
            skipped: BTreeSet::new(),
        }
    }

    /// Lower bound as a signed index: -1 when nothing is known green.
    fn lo(&self) -> i64 {
        self.green.map_or(-1, |g| g as i64)
    }

    /// Indices strictly between the known green and the known red, not skipped.
    fn open(&self) -> Vec<usize> {
        let first = (self.lo() + 1) as usize;
        (first..self.red)
            .filter(|i| !self.skipped.contains(i))
            .collect()
    }

    /// Candidates not in `exclude` that changed the map, over the whole range.
    fn also_suspect(&self, exclude: &[&str]) -> Vec<String> {
        self.candidates
            .iter()
            .filter(|c| c.changes_map && !exclude.contains(&c.sha.as_str()))
            .map(|c| c.sha.clone())
            .collect()
    }

    pub fn next(&self) -> Next {
        if self.candidates.is_empty() {
            return Next::Done(Verdict::NoCandidates);
        }
        let open = self.open();
        if open.is_empty() {
            let first = (self.lo() + 1) as usize;
            let suspects: Vec<&str> = self.candidates[first..=self.red]
                .iter()
                .map(|c| c.sha.as_str())
                .collect();
            let also_suspect = self.also_suspect(&suspects);
            return Next::Done(if let [only] = suspects[..] {
                Verdict::Culprit {
                    sha: only.to_string(),
                    also_suspect,
                }
            } else {
                Verdict::Inconclusive {
                    candidates: suspects.iter().map(|s| (*s).to_string()).collect(),
                    also_suspect,
                }
            });
        }
        let mid = (self.lo() + self.red as i64).div_euclid(2);
        // `min_by_key` keeps the first minimum, and `open` is ascending: ties go low.
        let pick = open
            .iter()
            .copied()
            .min_by_key(|&i| (i as i64 - mid).abs())
            .expect("open is not empty");
        Next::Probe(self.candidates[pick].sha.clone())
    }

    pub fn record(&mut self, sha: &str, probe: Probe) -> Result<(), RecordError> {
        let index = self
            .open()
            .into_iter()
            .find(|&i| self.candidates[i].sha == sha)
            .ok_or_else(|| RecordError::NotAsked(sha.to_string()))?;
        match probe {
            Probe::Green => self.green = Some(index),
            Probe::Red => self.red = index,
            Probe::Inconclusive => {
                self.skipped.insert(index);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands(names: &[&str]) -> Vec<Candidate> {
        names
            .iter()
            .map(|n| Candidate {
                sha: (*n).to_string(),
                changes_map: false,
            })
            .collect()
    }

    fn eight() -> Vec<Candidate> {
        cands(&["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7"])
    }

    fn probe_of(n: Next) -> String {
        match n {
            Next::Probe(s) => s,
            other => panic!("expected a probe, got {other:?}"),
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_candidates_is_its_own_verdict() {
        let b = Bisection::new(Vec::new());
        assert_eq!(b.next(), Next::Done(Verdict::NoCandidates));
    }

    #[test]
    fn a_single_candidate_is_the_culprit_without_a_probe() {
        let b = Bisection::new(cands(&["only"]));
        assert_eq!(
            b.next(),
            Next::Done(Verdict::Culprit {
                sha: "only".to_string(),
                also_suspect: Vec::new()
            })
        );
    }

    #[test]
    fn bisection_finds_the_first_red_in_log_steps() {
        // The fixed sequence: red from c5 is found by probing c3, c5, c4.
        let mut b = Bisection::new(eight());
        assert_eq!(probe_of(b.next()), "c3");
        b.record("c3", Probe::Green).unwrap();
        assert_eq!(probe_of(b.next()), "c5");
        b.record("c5", Probe::Red).unwrap();
        assert_eq!(probe_of(b.next()), "c4");
        b.record("c4", Probe::Green).unwrap();
        assert_eq!(
            b.next(),
            Next::Done(Verdict::Culprit {
                sha: "c5".to_string(),
                also_suspect: Vec::new()
            })
        );

        // Every possible first-red position is found in at most three probes.
        for first_red in 0..8usize {
            let mut b = Bisection::new(eight());
            let mut probes = 0;
            let verdict = loop {
                match b.next() {
                    Next::Probe(sha) => {
                        probes += 1;
                        let idx: usize = sha[1..].parse().unwrap();
                        let p = if idx >= first_red {
                            Probe::Red
                        } else {
                            Probe::Green
                        };
                        b.record(&sha, p).unwrap();
                    }
                    Next::Done(v) => break v,
                }
            };
            assert!(probes <= 3, "first_red={first_red} took {probes} probes");
            assert_eq!(
                verdict,
                Verdict::Culprit {
                    sha: format!("c{first_red}"),
                    also_suspect: Vec::new()
                }
            );
        }
    }

    #[test]
    fn an_inconclusive_probe_is_skipped_for_its_neighbour() {
        let mut b = Bisection::new(eight());
        assert_eq!(probe_of(b.next()), "c3");
        b.record("c3", Probe::Inconclusive).unwrap();
        // The tie between c2 and c4 goes to the lower index.
        assert_eq!(probe_of(b.next()), "c2");
    }

    #[test]
    fn skips_hiding_the_boundary_end_inconclusive() {
        let mut b = Bisection::new(cands(&["a", "b", "c"]));
        assert_eq!(probe_of(b.next()), "a");
        b.record("a", Probe::Green).unwrap();
        assert_eq!(probe_of(b.next()), "b");
        b.record("b", Probe::Inconclusive).unwrap();
        assert_eq!(
            b.next(),
            Next::Done(Verdict::Inconclusive {
                candidates: strs(&["b", "c"]),
                also_suspect: Vec::new()
            })
        );
    }

    #[test]
    fn a_map_changing_merge_is_always_suspect() {
        let mut list = cands(&["a", "b", "c"]);
        list[0].changes_map = true;
        let mut b = Bisection::new(list);
        assert_eq!(probe_of(b.next()), "a");
        b.record("a", Probe::Green).unwrap();
        assert_eq!(probe_of(b.next()), "b");
        b.record("b", Probe::Green).unwrap();
        assert_eq!(
            b.next(),
            Next::Done(Verdict::Culprit {
                sha: "c".to_string(),
                also_suspect: strs(&["a"])
            })
        );
    }

    #[test]
    fn recording_a_sha_not_asked_is_refused() {
        let mut b = Bisection::new(eight());
        assert_eq!(
            b.record("zzz", Probe::Green),
            Err(RecordError::NotAsked("zzz".to_string()))
        );
        b.record("c3", Probe::Green).unwrap();
        // Already resolved: no longer open.
        assert_eq!(
            b.record("c3", Probe::Green),
            Err(RecordError::NotAsked("c3".to_string()))
        );
        // Below the green boundary: not open either.
        assert_eq!(
            b.record("c1", Probe::Red),
            Err(RecordError::NotAsked("c1".to_string()))
        );
    }
}
