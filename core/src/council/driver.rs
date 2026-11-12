//! Council phase progression and pure tally-input helpers.

use super::*;

impl Driver {
    pub(super) async fn run(self) {
        let answers = self.answer_phase().await;
        if self.was_settled().await {
            return;
        }

        // The shuffle is over the seats that answered: a seat with no answer has nothing to be
        // judged on, so it is neither shown to a peer nor asked to judge one — it gets no step
        // after its failed answer.
        let participants: Vec<usize> = answers.keys().copied().collect();
        let anon = anonymize(&self.id, &participants);
        if let Err(error) = set_anon_map(&self.state.pool, &self.id, &anon.anon_map).await {
            tracing::warn!(council = %self.id, %error, "could not record the anonymisation map");
        }

        // The answer each seat currently stands behind. Starts as its round-0 answer and is replaced
        // only by a revision that claimed a change and said why; the round-0 step itself is never
        // rewritten, so what a seat first said stays readable beside what it ended up saying.
        let mut current = answers;
        let mut changes: Vec<(String, String)> = Vec::new();
        let mut rounds_run = 0;
        let mut stopped_early = false;
        let mut last_critiques = BTreeMap::new();
        // The previous round's Borda order and contested set, for the early-stop rule.
        let mut previous: Option<(Vec<usize>, std::collections::BTreeSet<usize>)> = None;

        for round in 1..=self.rounds {
            let Some(critiques) = self.critique_phase(round, &current, &anon).await else {
                break;
            };
            rounds_run = round;
            last_critiques = critiques;

            // One ballot is not a vote: below two readable critiques there is nothing to revise
            // against and nothing to compare the next round with, so the chairman takes it from here.
            let mut stop = last_critiques.len() < 2;
            if !stop {
                let ballots = ballots_of(&last_critiques);
                let order: Vec<usize> = tally::borda(&ballots, &anon.anon_map)
                    .into_iter()
                    .map(|row| row.seat_idx)
                    .collect();
                let disputed = tally::contested(&disputes_of(&last_critiques), &anon.anon_map);
                // "Early" means before the last round asked for: a council that ran every round it
                // was given did not stop early, whatever the last two rounds looked like.
                if round < self.rounds
                    && let Some((previous_order, previous_disputed)) = &previous
                    && tally::should_stop_early(
                        previous_order,
                        previous_disputed,
                        &order,
                        &disputed,
                    )
                {
                    stopped_early = true;
                    stop = true;
                }
                previous = Some((order, disputed));
            }

            if let Err(error) =
                store::set_progress(&self.state.pool, &self.id, rounds_run, stopped_early).await
            {
                tracing::warn!(council = %self.id, %error, "could not record the council's progress");
            }
            if self.was_settled().await {
                return;
            }
            if stop || round == self.rounds {
                break;
            }

            let changed = self
                .revise_phase(round, &mut current, &anon, &last_critiques)
                .await;
            changes.extend(changed);
            if self.was_settled().await {
                return;
            }
        }

        // A cancel that landed during the last critique/revise phase must not start a chairman
        // nothing would then kill.
        if self.was_settled().await {
            return;
        }
        self.chairman_phase(rounds_run, &current, &anon, &last_critiques, changes)
            .await;
    }

    /// A revise round — every seat that answered is shown its own current answer and the points
    /// its anonymous reviewers made about it in critique round `round`, and decides whether to
    /// change it.
    ///
    /// Recorded at the same `round` as the critique it answers. Never shown: a peer's answer, a
    /// model, a label or any standing — a seat that knew where it placed would revise towards the
    /// winner rather than towards the argument. No tools, for the reason the critique has none.
    ///
    /// A changed revision replaces the seat's entry in `current`; an unchanged one, an unreadable
    /// one and a failed run all leave it as it was. Returns `(seat name, why)` for each change.
    async fn revise_phase(
        &self,
        round: i64,
        current: &mut BTreeMap<usize, String>,
        anon: &Anonymized,
        critiques: &BTreeMap<usize, formats::Critique>,
    ) -> Vec<(String, String)> {
        self.move_to(round, store::PHASE_REVISE).await;

        let revising = current.iter().filter_map(|(seat_idx, own)| {
            let seat = self.members.get(*seat_idx)?;
            let received = points_received(*seat_idx, critiques, &anon.anon_map);
            let prompt =
                prompts::revise_prompt(&self.question, self.role_of(*seat_idx), own, &received);
            Some(async move {
                let slot = Slot::Step {
                    round,
                    seat_idx: *seat_idx,
                    phase: store::PHASE_REVISE,
                    shown: Vec::new(),
                };
                let outcome = self.run_seat(slot, seat, prompt, false).await;
                (*seat_idx, outcome)
            })
        });
        let outcomes = crate::join::all(revising).await;

        // Parsed again rather than handed back by `record`, as in the critique phase: the parse is
        // pure, so the answer that stands and the payload the step stores cannot differ.
        let mut changes = Vec::new();
        for (seat_idx, outcome) in outcomes {
            if outcome.status != SEAT_OK {
                continue;
            }
            if let Ok(formats::Revision {
                changed: true,
                answer: Some(answer),
                why,
            }) = formats::parse_revision(&outcome.answer)
            {
                current.insert(seat_idx, answer);
                changes.push((self.seat_name(seat_idx), why.unwrap_or_default()));
            }
        }

        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council revision after round {round}: {} of {} seats changed their answer",
                changes.len(),
                current.len()
            ),
            None,
            Some(&crate::feed::Subject::Council(self.id.clone())),
        )
        .await;

        changes
    }

    /// Moves the council's position, warning rather than failing: the position is what a reader is
    /// shown, never what the driver decides from.
    async fn move_to(&self, round: i64, phase: &str) {
        if let Err(error) = store::set_position(&self.state.pool, &self.id, round, phase).await {
            tracing::warn!(council = %self.id, %error, round, phase, "could not move the council");
        }
    }

    /// The name the chairman knows a seat by: its agent's, or the model's when no agent took it.
    fn seat_name(&self, seat_idx: usize) -> String {
        match self.members.get(seat_idx) {
            Some(seat) => match &seat.agent {
                Some(agent) => agent.name.clone(),
                None => seat.model_ref.clone(),
            },
            None => format!("seat {seat_idx}"),
        }
    }

    fn role_of(&self, seat_idx: usize) -> Option<formats::Role> {
        self.roles.get(seat_idx).copied().flatten()
    }

    /// Whether somebody has already closed this council — a cancel, or a write that lost a race.
    ///
    /// Checked at every phase boundary instead of the driver holding an abort handle of its own.
    /// Cancelling terminates the seat runs, which is what makes the phase in flight END; this is
    /// what stops the phase AFTER it from starting. A row that cannot be read is treated as settled,
    /// because continuing to spend against a record the daemon cannot see is the worse direction.
    async fn was_settled(&self) -> bool {
        match get_council_row(&self.state.pool, &self.id).await {
            Ok(Some(row)) => row.status != STATUS_RUNNING,
            Ok(None) => true,
            Err(error) => {
                tracing::warn!(council = %self.id, %error, "could not read the council; stopping");
                true
            }
        }
    }

    /// Round 0 — every seat answers the owner's question, all at once, behind its role.
    ///
    /// Returns the valid answers by `seat_idx`.
    async fn answer_phase(&self) -> BTreeMap<usize, String> {
        self.move_to(0, store::PHASE_ANSWER).await;
        let running = self.members.iter().enumerate().map(|(seat_idx, seat)| {
            let prompt = prompts::answer_prompt(&self.question, self.role_of(seat_idx));
            // With tools: this is the phase where a seat goes and finds what it needs. The critique
            // and the chairman get none, so nobody can go looking for ammunition after seeing a
            // peer's answer.
            async move {
                let slot = Slot::Step {
                    round: 0,
                    seat_idx,
                    phase: store::PHASE_ANSWER,
                    shown: Vec::new(),
                };
                let outcome = self.run_seat(slot, seat, prompt, true).await;
                (seat_idx, outcome)
            }
        });
        let outcomes = crate::join::all(running).await;

        let mut answers = BTreeMap::new();
        for (seat_idx, outcome) in outcomes {
            if outcome.status == SEAT_OK {
                answers.insert(seat_idx, outcome.answer);
            }
        }

        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council answers done: {} of {} seats answered",
                answers.len(),
                self.members.len()
            ),
            None,
            Some(&crate::feed::Subject::Council(self.id.clone())),
        )
        .await;

        answers
    }

    /// A critique round — each seat that answered reviews and ranks its peers' answers under
    /// shuffled labels, never its own.
    ///
    /// Returns the critiques that could be read, by the critic's `seat_idx`, or `None` when the round
    /// did not run. Below two valid answers it is skipped whole, and skipping is recorded rather
    /// than left blank: a `skipped` step and a `pending` one mean different things, and only one of
    /// them is a council that stopped. A critique that ran and could not be read is an ABSTENTION —
    /// one ballot fewer, recorded `invalid` with the reason — not a failed council.
    async fn critique_phase(
        &self,
        round: i64,
        answers: &BTreeMap<usize, String>,
        anon: &Anonymized,
    ) -> Option<BTreeMap<usize, formats::Critique>> {
        if !critique_should_run(answers.len()) {
            for seat_idx in answers.keys() {
                if let Err(error) = store::upsert_step(
                    &self.state.pool,
                    &self.id,
                    round,
                    *seat_idx as i64,
                    store::PHASE_CRITIQUE,
                    None,
                    SEAT_SKIPPED,
                    None,
                    None,
                )
                .await
                {
                    tracing::warn!(council = %self.id, %error, "could not record a skipped critique");
                }
            }
            return None;
        }

        self.move_to(round, store::PHASE_CRITIQUE).await;

        let critiquing = anon.for_seat.iter().filter_map(|(viewer, labels)| {
            let seat = self.members.get(*viewer)?;
            let peers: Vec<(String, String)> = labels
                .iter()
                .filter_map(|label| {
                    let seat_idx = anon.anon_map.get(label)?;
                    Some((label.clone(), answers.get(seat_idx)?.clone()))
                })
                .collect();
            let shown: Vec<String> = peers.iter().map(|(label, _)| label.clone()).collect();
            let prompt = prompts::critique_prompt(&self.question, self.role_of(*viewer), &peers);
            Some(async move {
                // No tools. A seat that could fetch targeted evidence after reading its peers'
                // answers would turn the critique into a measure of who had time left.
                let slot = Slot::Step {
                    round,
                    seat_idx: *viewer,
                    phase: store::PHASE_CRITIQUE,
                    shown: shown.clone(),
                };
                let outcome = self.run_seat(slot, seat, prompt, false).await;
                (*viewer, shown, outcome)
            })
        });
        let outcomes = crate::join::all(critiquing).await;

        // Parsed again rather than handed back by `record`: the parse is pure, so the ballot the
        // tally reads and the payload the step stores cannot differ.
        let mut critiques = BTreeMap::new();
        for (seat_idx, shown, outcome) in outcomes {
            if outcome.status == SEAT_OK
                && let Ok(critique) = formats::parse_critique(&outcome.answer, &shown)
            {
                critiques.insert(seat_idx, critique);
            }
        }

        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council round {round} of {}: {} of {} critiques read",
                self.rounds,
                critiques.len(),
                answers.len()
            ),
            None,
            Some(&crate::feed::Subject::Council(self.id.clone())),
        )
        .await;

        Some(critiques)
    }

    /// The last phase — the chairman writes the council's synthesis, as structured JSON.
    ///
    /// A chairman RUN that fails is the one failure that settles the council as `error`, and it is
    /// survivable in the way the others are not: the answers and critiques are still readable, and
    /// they are the part with value.
    ///
    /// A chairman that ran and wrote something unreadable is a different fact and does not fail the
    /// council. It is asked ONCE more, with the first prompt plus the reason the first reply was
    /// refused; if that also cannot be used, its raw text is kept as a `degraded` synthesis — shown
    /// as it came, with the reason — rather than thrown away.
    ///
    /// `answers` are the final answers by seat — the latest each seat stood behind — and
    /// `critiques` the last round's. A seat whose answer failed is absent, so the chairman is never
    /// asked to weigh a position nobody stated. `changes` is every revision that changed an answer,
    /// as `(seat name, why)`, across every round.
    async fn chairman_phase(
        &self,
        rounds_run: i64,
        answers: &BTreeMap<usize, String>,
        anon: &Anonymized,
        critiques: &BTreeMap<usize, formats::Critique>,
        changes: Vec<(String, String)>,
    ) {
        self.move_to(rounds_run, PHASE_CHAIRMAN).await;

        let ballots = ballots_of(critiques);
        let agreement = tally::agreement(&ballots, &anon.anon_map);
        // A seat no ballot scored has no standing to report; listing it at 0.00 would read as
        // "ranked last" when the truth is "never ranked".
        let leaderboard_lines: Vec<String> = tally::borda(&ballots, &anon.anon_map)
            .into_iter()
            .filter(|row| row.n > 0)
            .enumerate()
            .map(|(place, row)| {
                format!(
                    "{}. {} — {:.2}, {} vote(s)",
                    place + 1,
                    self.leaderboard_name(row.seat_idx),
                    row.score,
                    row.n
                )
            })
            .collect();

        let input = prompts::ChairmanInput {
            question: self.question.clone(),
            answers: answers
                .iter()
                .map(|(seat_idx, answer)| prompts::SeatBrief {
                    seat_idx: *seat_idx,
                    name: self.seat_name(*seat_idx),
                    role: self.role_of(*seat_idx),
                    answer: answer.clone(),
                })
                .collect(),
            critiques: critique_counts(critiques, &anon.anon_map),
            leaderboard_lines,
            agreement_level: agreement.level.clone(),
            changes,
        };
        let seats: std::collections::BTreeSet<usize> = answers.keys().copied().collect();
        let judge = |raw: &str| -> Result<formats::Synthesis, String> {
            let synthesis = formats::parse_synthesis(raw)?;
            formats::validate_synthesis(&synthesis, &seats, &agreement.level)?;
            Ok(synthesis)
        };

        let first = match self
            .ask_chairman(prompts::chairman_prompt(&input, None))
            .await
        {
            Ok(raw) => raw,
            Err((status, error)) => return self.settle(rounds_run, status, error).await,
        };
        let (synthesis, synthesis_status) = match judge(&first) {
            Ok(synthesis) => (synthesis, SYNTHESIS_OK),
            Err(reason) => {
                if self.was_settled().await {
                    return;
                }
                match self
                    .ask_chairman(prompts::chairman_prompt(&input, Some(&reason)))
                    .await
                {
                    Ok(second) => match judge(&second) {
                        Ok(synthesis) => (synthesis, SYNTHESIS_OK),
                        Err(reason) => (formats::degraded(&second, &reason), SYNTHESIS_DEGRADED),
                    },
                    Err((status, _)) if status == STATUS_CANCELLED => {
                        return self.settle(rounds_run, status, None).await;
                    }
                    // The retry's RUN failed, but the first reply is still text the chairman
                    // wrote: kept as degraded, which is more than an `error` council would show.
                    Err((_, error)) => (
                        formats::degraded(
                            &first,
                            &format!(
                                "{reason}; the retry failed: {}",
                                error.as_deref().unwrap_or("no reason given")
                            ),
                        ),
                        SYNTHESIS_DEGRADED,
                    ),
                }
            }
        };

        let encoded = serde_json::to_string(&synthesis).ok();
        if let Err(error) = store::set_synthesis(
            &self.state.pool,
            &self.id,
            encoded.as_deref(),
            synthesis_status,
        )
        .await
        {
            tracing::warn!(council = %self.id, %error, "could not record the synthesis");
        }
        self.settle(rounds_run, STATUS_DONE, None).await;
    }

    /// The leaderboard's name for a seat: the agent with its model beside it, or the bare model.
    fn leaderboard_name(&self, seat_idx: usize) -> String {
        match self.members.get(seat_idx) {
            Some(seat) => match &seat.agent {
                Some(agent) => format!("{} ({})", agent.name, seat.model_ref),
                None => seat.model_ref.clone(),
            },
            None => format!("seat {seat_idx}"),
        }
    }

    /// One chairman run: its reply, or how the council settles when the run itself did not answer.
    async fn ask_chairman(&self, prompt: String) -> Result<String, (&'static str, Option<String>)> {
        let outcome = self
            .run_seat(Slot::Chairman, &self.chairman, prompt, false)
            .await;
        if outcome.status == SEAT_OK {
            Ok(outcome.answer)
        } else if outcome.status == SEAT_CANCELLED {
            Err((STATUS_CANCELLED, None))
        } else {
            Err((
                STATUS_ERROR,
                Some(
                    outcome
                        .error
                        .unwrap_or_else(|| "the chairman produced no synthesis".to_string()),
                ),
            ))
        }
    }

    /// Settles the council, first writer wins, and says so in the feed when this call was it.
    async fn settle(&self, rounds_run: i64, status: &str, error: Option<String>) {
        if status != STATUS_CANCELLED {
            // Guarded on `running` like every position write, so it is a no-op for a council a
            // cancel has already settled.
            self.move_to(rounds_run, PHASE_DONE).await;
        }
        match finish(&self.state.pool, &self.id, status, error.as_deref()).await {
            Ok(true) => {
                let _ = crate::feed::append(
                    &self.state.pool,
                    None,
                    "council_finished",
                    &format!("council {status}"),
                    None,
                    Some(&crate::feed::Subject::Council(self.id.clone())),
                )
                .await;
            }
            // Somebody settled it first — a cancel, almost always. Their verdict stands.
            Ok(false) => {}
            Err(error) => {
                tracing::error!(council = %self.id, %error, "could not settle the council")
            }
        }
    }
}

/// PURE: what one step's row says, given how its run ended — `(status, error, payload)`.
///
/// An answer that came back `ok` is stored as `{"answer": text}`. A critique that came back `ok` is
/// parsed against the labels the seat was shown: what parses is stored as the ballot, already
/// narrowed to those labels; what does not is `invalid`, with the parser's reason — the run
/// finished, so its id stays on the step and the transcript stays reachable. Every other ending
/// keeps its own status and error, and carries no payload.
pub(super) fn step_record(
    phase: &str,
    shown: &[String],
    outcome: &SeatOutcome,
) -> (&'static str, Option<String>, Option<String>) {
    if outcome.status != SEAT_OK {
        return (outcome.status, outcome.error.clone(), None);
    }
    if phase == store::PHASE_CRITIQUE {
        return match formats::parse_critique(&outcome.answer, shown) {
            Ok(critique) => (SEAT_OK, None, serde_json::to_string(&critique).ok()),
            Err(reason) => (store::STEP_INVALID, Some(reason), None),
        };
    }
    if phase == store::PHASE_REVISE {
        // Keeping an answer is a valid revision and is stored as one (`changed: false`, no answer);
        // only a reply that cannot be read is `invalid`, and the seat's previous answer stands.
        return match formats::parse_revision(&outcome.answer) {
            Ok(revision) => (SEAT_OK, None, serde_json::to_string(&revision).ok()),
            Err(reason) => (store::STEP_INVALID, Some(reason), None),
        };
    }
    let payload = formats::AnswerPayload {
        answer: outcome.answer.clone(),
    };
    (SEAT_OK, None, serde_json::to_string(&payload).ok())
}

/// PURE: a round's ballots, `critic seat -> ranked labels`, the shape `tally` reads.
fn ballots_of(critiques: &BTreeMap<usize, formats::Critique>) -> BTreeMap<usize, Vec<String>> {
    critiques
        .iter()
        .map(|(critic, critique)| (*critic, critique.ranking.clone()))
        .collect()
}

/// PURE: for each critic, every label it reviewed and whether any of its points disagreed — the
/// shape `tally::contested` reads.
fn disputes_of(
    critiques: &BTreeMap<usize, formats::Critique>,
) -> BTreeMap<usize, Vec<(String, bool)>> {
    critiques
        .iter()
        .map(|(critic, critique)| {
            let notes = critique
                .reviews
                .iter()
                .map(|review| {
                    let disagrees = review
                        .points
                        .iter()
                        .any(|point| point.stance == formats::Stance::Disagree);
                    (review.label.clone(), disagrees)
                })
                .collect();
            (*critic, notes)
        })
        .collect()
}

/// PURE: the points `seat_idx`'s answer received in one critique round, one entry per reviewer.
///
/// Only the points: the reviewer is anonymous to the seat it reviewed, exactly as the seat was to
/// its reviewer, so nothing here says who wrote them or how they ranked. A review of a label that
/// names no seat, or of the critic's own answer, is ignored — the critic was never shown it.
fn points_received(
    seat_idx: usize,
    critiques: &BTreeMap<usize, formats::Critique>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<Vec<formats::Point>> {
    critiques
        .iter()
        .filter(|(critic, _)| **critic != seat_idx)
        .flat_map(|(_, critique)| {
            critique
                .reviews
                .iter()
                .filter(|review| anon_map.get(&review.label) == Some(&seat_idx))
                .map(|review| review.points.clone())
        })
        .collect()
}

/// PURE: how the critiques landed on each answer — stance counts and the reasons given against it,
/// by the seat that wrote the answer.
///
/// A review of a label that names no seat, or of the critic's own answer, is ignored for the reason
/// `tally::borda` ignores it: the critic was never shown it.
fn critique_counts(
    critiques: &BTreeMap<usize, formats::Critique>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<prompts::SeatCritiques> {
    let mut counts: BTreeMap<usize, prompts::SeatCritiques> = BTreeMap::new();
    for (critic, critique) in critiques {
        for review in &critique.reviews {
            let Some(&seat) = anon_map.get(&review.label) else {
                continue;
            };
            if seat == *critic {
                continue;
            }
            let entry = counts
                .entry(seat)
                .or_insert_with(|| prompts::SeatCritiques {
                    seat_idx: seat,
                    agree: 0,
                    disagree: 0,
                    unsure: 0,
                    disagree_whys: Vec::new(),
                });
            for point in &review.points {
                match point.stance {
                    formats::Stance::Agree => entry.agree += 1,
                    formats::Stance::Disagree => {
                        entry.disagree += 1;
                        entry.disagree_whys.push(point.why.clone());
                    }
                    formats::Stance::Unsure => entry.unsure += 1,
                }
            }
        }
    }
    counts.into_values().collect()
}

/// The last of a stderr stream, for the seat's error column.
///
/// Bounded because a CLI that fails at startup can produce a great deal of it, and the column is
/// read in a table beside seven others. The TAIL rather than the head: what killed a process is at
/// the end of what it said.
pub(super) fn tail_of(text: &str) -> String {
    const LIMIT: usize = 600;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let tail: String = trimmed
        .chars()
        .skip(trimmed.chars().count() - LIMIT)
        .collect();
    format!("…{tail}")
}
