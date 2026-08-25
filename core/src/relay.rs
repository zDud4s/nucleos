//! Whether one conversation may hand a message to another.
//!
//! A conversation can already act on another's behalf — `assistant.rs` lets a turn address a
//! second conversation instead of answering in its own — and the question this module answers is
//! narrower than "should it": given the chain of conversations a message has already travelled
//! through, root first, may it travel one hop further into `to`? `admits` is PURE, the same shape
//! as `team_trigger::closes_a_cycle` cut loose from its pool: no row is read here, because the
//! caller already has the chain by the time it needs an answer, and a decision this small earns
//! nothing from a database round trip except a reason to mock one in every test.
//!
//! `MAX_RELAY_DEPTH` is the sibling of `team_trigger::MAX_TRIGGER_DEPTH` — the same number, the
//! same shape of guess, this time bounding how far a message may be handed on between
//! conversations instead of how far a department may trigger another. Departments get a second,
//! dynamic brake (`closes_a_cycle` cannot see a chain that leaves the graph and comes back) because
//! a trigger fires unattended, days apart, off a signal nobody is reading in order. A relay chain
//! has no such blind spot: every hop is a value in `chain`, seen whole, in one call — so one
//! function carries both checks, and there is no static half living apart from a dynamic one.
//!
//! Refusals rank themselves. `Itself` and `Cycle` are mutually exclusive by construction — one
//! looks only at the last element of the chain, the other at everything before it — and either one
//! beats `TooDeep` when both would apply, because a chain that has looped back on itself is a
//! wrong chain, not merely a long one, and saying so is more useful than counting its length.

/// How many relays a chain may carry before it is refused for length alone.
///
/// Counts relays, not conversations: a chain of `n` conversations already carries `n - 1` relays
/// (the sending conversation is element one, with no relay behind it yet), so the refusal is
/// `chain.len() > MAX_RELAY_DEPTH`, not `chain.len() >= MAX_RELAY_DEPTH` — a chain of exactly four
/// conversations, three relays deep, is still admitted. Three, the same number as
/// `team_trigger::MAX_TRIGGER_DEPTH` and for the same reason: a chain of three conversations
/// relaying into each other is a workflow; a fourth hop is the shape of a loop nobody drew.
pub const MAX_RELAY_DEPTH: usize = 3;

/// What a proposed relay was decided to be, and why, when it is refused.
///
/// Three refusals and not one boolean, because "no" without a reason is a dead end for whoever
/// reads it back — a person watching the chain, or a model deciding whether to try a different
/// conversation instead. `Cycle` carries `at` for the same reason: the chain may be long, and a
/// refusal that does not say where it closes sends its reader back through the whole thing by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The relay may proceed.
    Ok,
    /// `to` is the conversation already sending — the last element of `chain`.
    Itself,
    /// `to` already appears in `chain`, but not as its last element.
    Cycle { at: String },
    /// Refusing on length alone: `chain.len() > MAX_RELAY_DEPTH`, and `to` closes neither a cycle
    /// nor a relay to itself — those are named specifically, above, because they are the more
    /// useful thing to say when true.
    TooDeep,
}

/// Whether the conversation at the end of `chain` may relay a message into `to`.
///
/// `chain` is root first and INCLUDES the sending conversation, so a turn a person wrote — nothing
/// relayed yet — is the one-element chain `&[origin]`. `admits(&[origin], "B")` is therefore the
/// question "may the conversation a person is in hand this message to B", and a chain of length
/// four already reflects three relays that already happened.
///
/// An empty `chain` answers `Ok`. No caller in this codebase can produce one — the type permits it,
/// nothing does — and inventing a fifth `Verdict` for a shape that never occurs would be a case to
/// maintain forever against a bug that has never shown up. Zero relays is, truthfully, not a cycle,
/// not a relay to itself, and not too deep.
pub fn admits(chain: &[&str], to: &str) -> Verdict {
    // `Itself` and `Cycle` are decided before length is even considered — a chain that has looped
    // back on itself is a wrong chain, not merely a long one, and that is the more useful thing to
    // say even when the chain is also past `MAX_RELAY_DEPTH` (see the second pinned test below).
    if let Some(&last) = chain.last() {
        if to == last {
            return Verdict::Itself;
        }
        for &link in &chain[..chain.len() - 1] {
            if link == to {
                return Verdict::Cycle {
                    at: link.to_string(),
                };
            }
        }
    }
    if chain.len() > MAX_RELAY_DEPTH {
        return Verdict::TooDeep;
    }
    Verdict::Ok
}

/// The chain `admits` judges, reconstructed for the run at `run_id` rather than carried by a
/// caller who already has it in hand.
///
/// Root first, and INCLUDES `run_id`'s own conversation — the same convention `admits` documents
/// for `chain`, so a chain this function returns can be handed to `admits` unmodified. The walk
/// follows `runs.from_relay_id` to `chat_relays.sending_run_id` to that run's own `from_relay_id`,
/// and so on, stopping at a run whose `from_relay_id` is NULL: by construction (0117) that is a
/// turn a person wrote, not one handed on by another conversation, and every real chain bottoms
/// out there. `chat_relays.depth` is deliberately not read — it is convenience for a human reading
/// the table, not the authority the walk answers to; see 0117's header. The chain is DERIVED, never
/// stored, and this walk is what a caller trusts when it and `depth` could ever disagree.
///
/// **Bounded, not merely optimistic.** `admits` refuses, at write time, any relay that would carry
/// a chain past `MAX_RELAY_DEPTH`, so a *legitimate* chain never reaches more than
/// `MAX_RELAY_DEPTH + 1` conversations — that is the walk's own ceiling, borrowed rather than
/// guessed. A walk that has not reached a person's turn within that many hops did not get long by
/// any route `admits` ever allowed: it is a cycle among the rows, or a pointer a hand-edit left
/// dangling, and either way the walk stops there and returns `Err`, not the chain gathered so far.
/// Truncating silently was rejected on purpose: a truncated chain is a SHORTER chain, and `admits`
/// reads a short chain as one safe to relay through — silence here would let a malformed chain pass
/// as an admissible one, which is the direction that fails OPEN.
///
/// **An unknown `run_id` answers `RowNotFound`, not an empty chain.** `Vec::new()` is not a shape
/// this walk can honestly return for a live run — even the shallowest real chain, a person's own
/// turn with nothing relayed yet, is one element — so an empty result would read as "relayed from
/// nothing" when the truth is "no such run", two facts a caller must never be left to confuse.
/// Every row the walk expects and does not find — the starting run, or a `from_relay_id` /
/// `sending_run_id` pointer partway through — surfaces the same way, through `fetch_one`'s own
/// `RowNotFound`, rather than a second bespoke error for the same fact.
pub async fn chain_of(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<Vec<String>> {
    // The honest ceiling, not an arbitrary one: `MAX_RELAY_DEPTH + 1` conversations is the longest
    // chain `admits` could ever have let get written, so needing one more hop than that means the
    // data is wrong, not merely large — see the doc comment above.
    const CEILING: usize = MAX_RELAY_DEPTH + 1;

    let mut chain = Vec::new();
    let mut current_run_id = run_id;

    loop {
        if chain.len() >= CEILING {
            return Err(sqlx::Error::Protocol(format!(
                "chat relay chain for run {run_id} did not reach a person's turn within \
                 {CEILING} conversations — refusing it rather than returning a chain truncated to \
                 look shorter than it is"
            )));
        }

        let (chat_id, from_relay_id): (String, Option<i64>) =
            sqlx::query_as("SELECT chat_id, from_relay_id FROM runs WHERE id = ?")
                .bind(current_run_id)
                .fetch_one(pool)
                .await?;
        chain.push(chat_id);

        let Some(relay_id) = from_relay_id else {
            break;
        };
        current_run_id = sqlx::query_scalar("SELECT sending_run_id FROM chat_relays WHERE id = ?")
            .bind(relay_id)
            .fetch_one(pool)
            .await?;
    }

    chain.reverse();
    Ok(chain)
}

/// Why `admit` refused to write a relay.
///
/// One variant per cause, each carrying whatever it takes to name itself — the same rule
/// `team_trigger::closes_a_cycle` already follows for its own cycle. A refusal that only says "no"
/// sends its reader back through the whole gate by hand to work out which brake actually caught it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `to_chat_id` names no live conversation — never created, or archived. The two are not told
    /// apart on purpose: `chats::brain_of`, and everything else that reads `chats`, already treats
    /// an archived row as gone (`archived_at IS NULL`), so a relay follows the same rule rather than
    /// inventing a second meaning of "absent" that only this module believes.
    NoSuchDestination { to_chat_id: String },
    /// The sending turn's `origin` is `Origin::Telegram`. Named on its own rather than left to fall
    /// out of some other rule, because a boundary that only holds by accident of what else happens
    /// to be configured is not one a reader can trust — see this module's header.
    TelegramOrigin,
    /// The chain itself refuses the hop. Carried whole, not re-decomposed into new variants:
    /// `Verdict` already names `Itself`, `Cycle { at }` and `TooDeep` as specifically as they can be
    /// named, and repeating that shape here would be a second definition of the same fact, free to
    /// drift out of step with `admits`.
    Chain(Verdict),
    /// `attention::owner_is_present` answered false for `now`.
    OwnerAway,
    /// A read or write this gate depends on failed outright — sqlx itself broke, not a verdict
    /// about the relay. `step` names which one so a reader of a log can tell "the destination
    /// could not be looked up" from "the chain could not be walked" without parsing `error`'s
    /// text; `error` keeps sqlx's own message, since none of the three call sites below has
    /// anything truer to say about a failure they did not cause.
    ///
    /// **Not the same fact as `NoSuchDestination`.** That variant means the destination lookup
    /// SUCCEEDED and found nothing — an archived or a never-created conversation, a legitimate
    /// empty answer. This variant means the lookup itself failed and the answer is unknown;
    /// collapsing the two would let an infrastructure failure read back as a deliberate, ordinary
    /// refusal. It is the same distinction `attention::owner_is_present` draws between "no
    /// heartbeat recorded" (a valid empty state, allowed) and "the read itself broke" (fails
    /// closed) — see that module's header.
    ///
    /// **The write below reaches this variant for a different reason than the two reads above.**
    /// A failed read means the gate could not yet decide whether to admit. A failed INSERT means
    /// the gate had ALREADY decided to admit and then failed to record that decision — and
    /// because `runs.from_relay_id` goes on to point at the row that INSERT was writing, an
    /// admitted relay that was never recorded is precisely the dangling pointer `chain_of` can
    /// neither see nor walk through. Refusing is still correct here — the caller must never be
    /// handed an id that names no row — but it is refusing a different failure than "could not
    /// decide", not the same fact wearing two names.
    Unreadable { step: &'static str, error: String },
}

/// The gate: whether a message may travel from `from_chat_id`, sent by the turn `sending_run_id`,
/// on into `to_chat_id` — and, if so, the id of the `chat_relays` row recording that it did.
///
/// Four brakes, checked in the order below, each cheaper or more specific than the next: existence,
/// then origin, then the chain, then attention. Only the first one that refuses is ever reported —
/// a caller is never told about a second reason a relay already impossible for a first, cheaper
/// reason would also have failed.
///
/// **No budget brake here, on purpose.** A relay that clears every check below can still be one the
/// owner would not have paid for, and that ceiling belongs with the rest of spend accounting in
/// `budget.rs` — see that module's own change. Bolting a budget check onto a module that otherwise
/// never touches money would give `relay.rs` a second, narrower opinion about spend that
/// `budget.rs` cannot see or reconcile with its own; leaving it out here is deliberate, not
/// forgotten.
///
/// **`depth`, on the row this writes, is `chain.len()` — the chain as it stood before this hop, not
/// after.** 0117's worked example is the check: the first relay out of a person's turn (a chain of
/// one) is written with `depth = 1`, the next with `depth = 2`, and so on. It is read back by
/// nobody this function trusts; `chain_of` re-derives the true chain on every read, per that
/// migration's own header, so this column can only ever be a courtesy to a person skimming the
/// table, never the thing a caller relies on.
///
/// **`body` is written, and the row is worth reading because of it.** This parameter did not exist
/// when `admit` was first written: there was no caller composing relay text, so every row went in
/// with the empty string. `http::relay_send_to_chat` has held that text all along, which made
/// `chat_relays` a table recording that SOMETHING was relayed and never what — an audit trail whose
/// one interesting column was blank on every row. Passed in rather than read back out of the
/// destination's own turn afterwards: a queued relay has no turn yet, and one that is refused
/// downstream never gets one, so the message would be unrecoverable in exactly the cases somebody
/// would go looking for it.
///
/// Not truncated, and not redacted. What a conversation said to another conversation is already in
/// `runs.prompt` on the receiving turn once one exists; this is the same words, in the row that
/// says where they came from, and shortening them here would make the two disagree.
pub async fn admit(
    pool: &sqlx::SqlitePool,
    from_chat_id: &str,
    to_chat_id: &str,
    sending_run_id: i64,
    origin: crate::assistant::Origin,
    body: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<i64, Refusal> {
    // Cheapest and least specific first: one lookup, no chain to walk yet. Reusing `chats::brain_of`
    // rather than a second `... AND archived_at IS NULL` clause means "still there" has exactly one
    // definition in this codebase, not two that a future edit could drift apart.
    //
    // A failure here is answered `Unreadable`, not `NoSuchDestination` — see that variant's doc
    // comment. `chats::brain_of` returning `Err` means the lookup itself broke, and the caller must
    // not be told the same thing it would hear for an archived or never-created conversation.
    let destination_lives = crate::chats::brain_of(pool, to_chat_id)
        .await
        .map_err(|error| Refusal::Unreadable {
            step: "destination lookup",
            error: error.to_string(),
        })?
        .is_some();
    if !destination_lives {
        return Err(Refusal::NoSuchDestination {
            to_chat_id: to_chat_id.to_string(),
        });
    }

    // Refused on its own line, not left to fall out of some other brake. A boundary that only holds
    // because no cheaper rule happened to catch a Telegram turn first would move with whatever else
    // is configured — a local model wired in tomorrow could quietly let one through — and a security
    // boundary nobody can point at is not one anybody can trust.
    if origin == crate::assistant::Origin::Telegram {
        return Err(Refusal::TelegramOrigin);
    }

    // The chain is the only brake below that needs more than the row already in hand: `chain_of`
    // does the one walk this function needs, and `admits` — PURE, and already proven by the table
    // above — is the sole judge of what that walk means.
    //
    // `chain_of` failing is the SECURITY case its own doc comment names: a malformed chain that
    // cannot be walked within `MAX_RELAY_DEPTH + 1` hops, or a `sending_run_id` that names no row
    // at all. The design states the rule for exactly this as a table: cannot read the chain →
    // Closed. `Unreadable` is that refusal — not a panic that takes the request handler down with
    // it, and not `Ok`, which would let a chain nobody could verify pass as one proven safe.
    let chain = chain_of(pool, sending_run_id)
        .await
        .map_err(|error| Refusal::Unreadable {
            step: "chain walk",
            error: error.to_string(),
        })?;
    let chain_refs: Vec<&str> = chain.iter().map(String::as_str).collect();
    let verdict = admits(&chain_refs, to_chat_id);
    if verdict != Verdict::Ok {
        return Err(Refusal::Chain(verdict));
    }

    // `owner_is_present`, deliberately, and not `attention_permits_new_run`. The latter defers
    // whenever ANY project-less run is in flight — and the turn sending this very relay IS one — so
    // calling it here would refuse every relay ever written, including the one about to clear every
    // other brake. Fallen into once already during design; `owner_is_present` asks the narrower
    // question this brake actually means: is anyone there to see the message land.
    if !crate::attention::owner_is_present(pool, now).await {
        return Err(Refusal::OwnerAway);
    }

    // Every brake above has now passed: this INSERT is not deciding whether to admit the relay,
    // it is recording a decision already made. A failure here still answers `Unreadable` — the
    // caller must never be handed an id that names no row — but it is a different fact from the
    // two reads above, not the same one under a shared name. A failed read means the gate could
    // not yet tell whether to admit. A failed write here means the gate already decided to admit
    // and then failed to record that it had: `runs.from_relay_id` will go on to point at this very
    // row, so an admitted relay that was never written is precisely the dangling pointer
    // `chain_of` cannot see or walk through on some later hop. See `Refusal::Unreadable`'s own doc
    // comment for the same distinction spelled out in full.
    let depth = chain.len() as i64;
    let relay_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(from_chat_id)
    .bind(to_chat_id)
    .bind(sending_run_id)
    .bind(body)
    .bind(depth)
    .bind(now.to_rfc3339())
    .fetch_one(pool)
    .await
    .map_err(|error| Refusal::Unreadable {
        step: "relay write",
        error: error.to_string(),
    })?;
    Ok(relay_id)
}

/// Records that `relay_id` became `run_id` — the mirror of `runs.from_relay_id`, written from the
/// other side once the receiving turn exists.
///
/// **Auditing, and its failure mode says so.** `Result` is returned rather than swallowed, but no
/// caller may treat an `Err` as a failed turn: 0117's header is explicit that the property this
/// module protects — that a chain is always reconstructible, and therefore always boundable —
/// hangs entirely on `runs.from_relay_id`, written in the same INSERT that creates the run.
/// `chain_of` walks that column and never this one. What is lost when this write fails is a
/// person's ability to ask "did this relay land, and where", which is worth a warning in the log
/// and is not worth undoing a turn that has already started.
///
/// A second statement, necessarily: the run's id does not exist until the run is inserted, and the
/// relay row was written before that, by `admit`, as the thing whose id the run then points at.
/// The window between them is the reason the two pointers are not equally trusted, and the reason
/// `delivered_to_run_id` being NULL is an ordinary state rather than a defect — the relay was
/// admitted and has not been claimed yet, or was queued and is still waiting, or was dropped by a
/// drain that found a brake newly engaged.
pub async fn mark_delivered(
    pool: &sqlx::SqlitePool,
    relay_id: i64,
    run_id: i64,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE chat_relays SET delivered_to_run_id = ? WHERE id = ?")
        .bind(run_id)
        .bind(relay_id)
        .execute(pool)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::Origin;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// Pins every case the semantics distinguish: the plain grant, refusing a conversation that
    /// would relay to itself, a cycle closing at two different depths, and the `TooDeep` boundary
    /// on both sides — a chain three relays deep still admitted, a fourth refused. Table-shaped
    /// because the interesting fact is the boundary between rows, not any single one of them.
    #[test]
    fn a_table_of_relay_decisions_matches_the_pinned_verdicts() {
        for (chain, to, expected, why) in [
            (
                vec!["A"],
                "B",
                Verdict::Ok,
                "a first relay, nothing behind it yet",
            ),
            (
                vec!["A"],
                "A",
                Verdict::Itself,
                "the sending conversation is also the target",
            ),
            (
                vec!["A", "B"],
                "A",
                Verdict::Cycle {
                    at: "A".to_string(),
                },
                "closes on the root of a two-conversation chain",
            ),
            (
                vec!["A", "B", "C"],
                "B",
                Verdict::Cycle {
                    at: "B".to_string(),
                },
                "closes partway through a three-conversation chain, not at the root",
            ),
            (
                vec!["A", "B", "C"],
                "D",
                Verdict::Ok,
                "three conversations is two relays already spent, still within MAX_RELAY_DEPTH",
            ),
            (
                vec!["A", "B", "C", "D"],
                "E",
                Verdict::TooDeep,
                "a fourth relay exceeds MAX_RELAY_DEPTH and closes nothing",
            ),
        ] {
            assert_eq!(admits(&chain, to), expected, "{why}");
        }
    }

    /// `Cycle` outranks `TooDeep` when a chain is both: a chain long enough to be refused on
    /// length is exactly the kind of chain likely to have looped back on itself too, and naming
    /// where it closes is the more useful refusal to hand back.
    #[test]
    fn a_cycle_is_named_even_in_a_chain_that_is_also_too_deep() {
        assert_eq!(
            admits(&["A", "B", "C", "D"], "B"),
            Verdict::Cycle {
                at: "B".to_string()
            }
        );
    }

    /// Builds the worked example from 0117's header by hand — a person's turn in A, relayed to B,
    /// relayed on to C — and checks that `chain_of` walks back to exactly that, root first. The
    /// second assertion is the base case rather than an afterthought: a chain of one IS what "a
    /// person wrote this, nothing relayed yet" looks like, and a walk that mishandled it would
    /// either loop forever or return nothing for the turn every real chain eventually reaches.
    #[tokio::test]
    async fn a_chain_is_walked_back_to_the_turn_a_person_wrote() {
        let pool = test_pool().await;

        let run_1 = sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, chat_id)
             VALUES ('hello', 'completed', '2026-08-22T00:00:00Z', 'A')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let relay_1 = sqlx::query(
            "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES ('A', 'B', ?, 'onward to B', 1, '2026-08-22T00:00:01Z')",
        )
        .bind(run_1)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let run_2 = sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, chat_id, from_relay_id)
             VALUES ('hello', 'completed', '2026-08-22T00:00:02Z', 'B', ?)",
        )
        .bind(relay_1)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let relay_2 = sqlx::query(
            "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES ('B', 'C', ?, 'onward to C', 2, '2026-08-22T00:00:03Z')",
        )
        .bind(run_2)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let run_3 = sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, chat_id, from_relay_id)
             VALUES ('hello', 'completed', '2026-08-22T00:00:04Z', 'C', ?)",
        )
        .bind(relay_2)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let chain = chain_of(&pool, run_3)
            .await
            .expect("walk the three-hop chain");
        assert_eq!(
            chain,
            vec!["A".to_string(), "B".to_string(), "C".to_string()],
            "root first, ending in the conversation run_3 actually belongs to"
        );

        let root_chain = chain_of(&pool, run_1)
            .await
            .expect("walk the root's own chain");
        assert_eq!(
            root_chain,
            vec!["A".to_string()],
            "a chain of one is the terminating case: a turn a person wrote, nothing relayed yet"
        );
    }

    // -- admit -----------------------------------------------------------------------------
    //
    // Every test below sets up every OTHER brake to be one `admit` would clear, and varies only
    // the one under test — so a refusal in a test proves that specific brake caught it, not that
    // some brake or other did. `admit`'s body is `todo!()` until GREEN, so every one of these
    // fails on that panic; the assertions after the call exist for GREEN to make true, not RED.

    fn timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A conversation `admit` can see. `archived` drives the same "gone" reading `chats::brain_of`
    /// gives an archived row — see `Refusal::NoSuchDestination`.
    async fn seed_chat(pool: &sqlx::SqlitePool, chat_id: &str, archived: bool) {
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, archived_at) VALUES (?, 'cloud', '2026-08-22T00:00:00Z', ?)",
        )
        .bind(chat_id)
        .bind(archived.then_some("2026-08-22T00:00:00Z"))
        .execute(pool)
        .await
        .unwrap();
    }

    /// Marks the owner present, globally, right now — the one row `attention::owner_is_present`
    /// reads. Omitting this call is how a test drives the away case: absence alone already answers
    /// false, by that function's own fail-closed design.
    async fn seed_owner_present(pool: &sqlx::SqlitePool, now: chrono::DateTime<chrono::Utc>) {
        sqlx::query(
            "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at) VALUES ('global', '', ?)",
        )
        .bind(now.to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    /// Writes a person's turn in `chat_ids[0]`, then one relay per remaining element of
    /// `chat_ids`, and hands back the id of the run at the end of it — ready to be passed to
    /// `admit` as `sending_run_id`. The chain `chain_of` would walk back from that run is
    /// `chat_ids` itself, root first, which is exactly the shape every test below needs and does
    /// not want to build by hand a second time.
    async fn seed_chain(pool: &sqlx::SqlitePool, chat_ids: &[&str]) -> i64 {
        let mut run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, created_at, chat_id) VALUES ('hello', 'completed', '2026-08-22T00:00:00Z', ?)",
        )
        .bind(chat_ids[0])
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();

        for pair in chat_ids.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let relay_id = sqlx::query(
                "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
                 VALUES (?, ?, ?, 'onward', 1, '2026-08-22T00:00:00Z')",
            )
            .bind(from)
            .bind(to)
            .bind(run_id)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid();

            run_id = sqlx::query(
                "INSERT INTO runs (prompt, status, created_at, chat_id, from_relay_id)
                 VALUES ('hello', 'completed', '2026-08-22T00:00:00Z', ?, ?)",
            )
            .bind(to)
            .bind(relay_id)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid();
        }

        run_id
    }

    /// Archived reads as absent, and the refusal repeats the chat id rather than leaving a reader
    /// to guess which conversation the gate meant.
    #[tokio::test]
    async fn an_archived_conversation_does_not_receive_relays() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_chat(&pool, "Z", true).await;
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A"]).await;

        let refusal = admit(&pool, "A", "Z", run_id, Origin::Shell, "relayed words", now)
            .await
            .expect_err("an archived destination must be refused");
        assert_eq!(
            refusal,
            Refusal::NoSuchDestination {
                to_chat_id: "Z".to_string()
            }
        );
    }

    /// Never having existed is indistinguishable from having been archived — both are "no live row
    /// in `chats`" — and `admit` refuses both the same way.
    #[tokio::test]
    async fn a_relay_to_a_conversation_that_was_never_created_is_refused() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A"]).await;

        let refusal = admit(
            &pool,
            "A",
            "ghost",
            run_id,
            Origin::Shell,
            "relayed words",
            now,
        )
        .await
        .expect_err("a destination that was never created must be refused");
        assert_eq!(
            refusal,
            Refusal::NoSuchDestination {
                to_chat_id: "ghost".to_string()
            }
        );
    }

    /// Refused outright, even with a live destination, an admitting chain and a present owner —
    /// this brake does not wait to be reached by elimination, see the header on `Refusal::TelegramOrigin`.
    #[tokio::test]
    async fn a_turn_from_telegram_may_not_relay_even_when_everything_else_would_allow_it() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_chat(&pool, "B", false).await;
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A"]).await;

        let refusal = admit(
            &pool,
            "A",
            "B",
            run_id,
            Origin::Telegram,
            "relayed words",
            now,
        )
        .await
        .expect_err("a Telegram-origin turn must be refused");
        assert_eq!(refusal, Refusal::TelegramOrigin);
    }

    /// A chain of `["A", "B"]` relaying back into `"A"` closes a cycle at the root, and the
    /// refusal names `"A"` rather than making its reader re-walk the chain to find where.
    #[tokio::test]
    async fn a_relay_that_would_close_a_cycle_is_refused_and_names_where_it_closes() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_chat(&pool, "B", false).await;
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A", "B"]).await;

        let refusal = admit(&pool, "B", "A", run_id, Origin::Shell, "relayed words", now)
            .await
            .expect_err("relaying back to the root must be refused as a cycle");
        assert_eq!(
            refusal,
            Refusal::Chain(Verdict::Cycle {
                at: "A".to_string()
            })
        );
    }

    /// `["A", "B", "C", "D"]` is already three relays deep — `MAX_RELAY_DEPTH` — so a fourth, to
    /// `"E"`, is refused for length alone: it closes nothing, it is simply one hop too many.
    #[tokio::test]
    async fn a_chain_already_at_max_relay_depth_is_refused_as_too_deep() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        for chat_id in ["A", "B", "C", "D", "E"] {
            seed_chat(&pool, chat_id, false).await;
        }
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A", "B", "C", "D"]).await;

        let refusal = admit(&pool, "D", "E", run_id, Origin::Shell, "relayed words", now)
            .await
            .expect_err("a fourth relay must be refused for length alone");
        assert_eq!(refusal, Refusal::Chain(Verdict::TooDeep));
    }

    /// A `sending_run_id` naming no row is exactly `chain_of`'s own documented `RowNotFound` case —
    /// its doc comment calls this walk's ceiling the SECURITY case the whole gate exists for, and a
    /// malformed or unreadable chain must read back as `Closed`, per the design table, not as a
    /// panic that takes down the request handler around it. No chain is seeded on purpose: this
    /// `run_id` is never written anywhere, so `chain_of` cannot find even its first row.
    #[tokio::test]
    async fn a_chain_that_cannot_be_read_refuses_rather_than_crashing() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "B", false).await;
        seed_owner_present(&pool, now).await;

        let refusal = admit(
            &pool,
            "A",
            "B",
            999_999,
            Origin::Shell,
            "relayed words",
            now,
        )
        .await
        .expect_err("a chain that cannot be read must be refused, not panic");
        match refusal {
            Refusal::Unreadable { step, .. } => assert_eq!(step, "chain walk"),
            other => panic!("expected Refusal::Unreadable naming the chain walk, got {other:?}"),
        }
    }

    /// No heartbeat row at all — `owner_is_present`'s own fail-closed default — refuses the relay
    /// even though the destination, origin and chain would all otherwise allow it.
    #[tokio::test]
    async fn an_absent_owner_refuses_the_relay() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_chat(&pool, "B", false).await;
        let run_id = seed_chain(&pool, &["A"]).await;

        let refusal = admit(&pool, "A", "B", run_id, Origin::Shell, "relayed words", now)
            .await
            .expect_err("an away owner must refuse the relay");
        assert_eq!(refusal, Refusal::OwnerAway);
    }

    /// Every brake clear: the happy path writes the `chat_relays` row rather than merely promising
    /// to, and the id `admit` hands back is the id of that exact row.
    #[tokio::test]
    async fn a_relay_that_clears_every_brake_writes_its_row_and_returns_its_id() {
        let pool = test_pool().await;
        let now = timestamp("2026-08-22T00:00:00Z");
        seed_chat(&pool, "A", false).await;
        seed_chat(&pool, "B", false).await;
        seed_owner_present(&pool, now).await;
        let run_id = seed_chain(&pool, &["A"]).await;

        let relay_id = admit(&pool, "A", "B", run_id, Origin::Shell, "relayed words", now)
            .await
            .expect("every brake is clear; the relay must be admitted");

        let (from_chat_id, to_chat_id, sending_run_id, body, delivered): (
            String,
            String,
            i64,
            String,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT from_chat_id, to_chat_id, sending_run_id, body, delivered_to_run_id
               FROM chat_relays WHERE id = ?",
        )
        .bind(relay_id)
        .fetch_one(&pool)
        .await
        .expect("admit must actually have written the row it claims to have written");

        assert_eq!(from_chat_id, "A");
        assert_eq!(to_chat_id, "B");
        assert_eq!(sending_run_id, run_id);
        // The words, and not the empty string this column held on every row until `admit` was given
        // something to write there. A table that records a relay happened and not what it said is
        // an audit trail with its one interesting column blank.
        assert_eq!(body, "relayed words");
        // Undelivered at this point, and that is the correct answer rather than a missing one:
        // `admit` decides and records the hop, and `relay::mark_delivered` stamps this from the
        // other side once a run exists to name. Pinned so the two never collapse into one write.
        assert_eq!(delivered, None);
    }
}
