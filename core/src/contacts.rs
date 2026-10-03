use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sqlx::{Sqlite, SqlitePool, Transaction};

/// What a correspondent looks like to `priority.rs`, which reads two of these five fields.
///
/// The other three carried `#[allow(dead_code)]` and the note "consumed by the later contact
/// display surface". That surface arrived — the Contacts tab, `GET /contacts` — and consumes none
/// of them: it has its own query (`list`) over the same tables. The note had stopped being true
/// while still reading like a plan, which is exactly the failure mode of a suppression that
/// describes instead of instructing.
///
/// They are not dead, though. Each is read by this module's own tests, and each pins a property of
/// `profile_for`'s SQL that nothing else reaches:
///
/// - `display_name` — the accumulated name survives the pruning of the message it came from, and a
///   human-set `contacts.display_name` beats the observed one.
/// - `first_seen` / `last_seen` — `MIN`/`MAX` widen the window from BOTH ends, so a message that
///   arrives out of order moves `first_seen` backwards without disturbing `last_seen`.
///
/// `emails` is a rolling thirty-day window, so "survives the prune" is the whole reason these facts
/// are accumulated at ingestion rather than read back off the mail. The suppression is therefore
/// scoped to the non-test build: under `cfg(test)` there is none, so a field that stopped being
/// read THERE — losing the coverage above — warns rather than going quiet.
pub struct Profile {
    #[cfg_attr(not(test), allow(dead_code))]
    pub display_name: Option<String>,
    pub messages_in: i64,
    #[cfg_attr(not(test), allow(dead_code))]
    pub first_seen: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub last_seen: String,
    pub outbound_ever: bool,
}

pub fn normalize_address(address: &str) -> String {
    let trimmed = address.trim();
    let address_only = trimmed
        .rfind('<')
        .and_then(|start| {
            trimmed[start + 1..]
                .find('>')
                .map(|end| &trimmed[start + 1..start + 1 + end])
        })
        .unwrap_or(trimmed);

    address_only.trim().to_lowercase()
}

/// Splits a recipient list on the commas that separate addresses.
///
/// Not every comma does: a display name may contain one, and `"Silva, Maria" <maria@x>` is one
/// recipient, not two.
/// A backslash-escaped display-name quote is the exact case that defeats a quote-aware splitter.
pub fn split_address_list(header: &str) -> Vec<&str> {
    let mut addresses = Vec::new();
    let mut start = 0;
    let mut inside_quotes = false;
    let mut inside_angles = false;
    let mut previous_was_backslash = false;

    for (index, character) in header.char_indices() {
        match character {
            '"' if !previous_was_backslash => inside_quotes = !inside_quotes,
            '<' => inside_angles = true,
            '>' => inside_angles = false,
            ',' if !inside_quotes && !inside_angles => {
                addresses.push(&header[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }

        previous_was_backslash = character == '\\';
    }

    addresses.push(&header[start..]);
    addresses
}

#[derive(Clone, Copy)]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

impl MessageDirection {
    fn update_statement(self) -> &'static str {
        match self {
            Self::Inbound => {
                "UPDATE contact_addresses
                 SET first_seen = MIN(first_seen, ?),
                     last_seen = MAX(last_seen, ?),
                     messages_in = messages_in + 1
                 WHERE address = ?"
            }
            Self::Outbound => {
                "UPDATE contact_addresses
                 SET first_seen = MIN(first_seen, ?),
                     last_seen = MAX(last_seen, ?),
                     outbound_ever = 1
                 WHERE address = ?"
            }
        }
    }

    fn insert_statement(self) -> &'static str {
        match self {
            Self::Inbound => {
                "INSERT INTO contact_addresses (
                     address, contact_id, first_seen, last_seen, messages_in, outbound_ever
                 )
                 VALUES (?, ?, ?, ?, 1, 0)"
            }
            Self::Outbound => {
                "INSERT INTO contact_addresses (
                     address, contact_id, first_seen, last_seen, messages_in, outbound_ever
                 )
                 VALUES (?, ?, ?, ?, 0, 1)"
            }
        }
    }
}

async fn record_message(
    transaction: &mut Transaction<'_, Sqlite>,
    address: &str,
    occurred_at: &str,
    direction: MessageDirection,
) -> sqlx::Result<()> {
    let address = normalize_address(address);
    let contact_id: Option<i64> =
        sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
            .bind(&address)
            .fetch_optional(&mut **transaction)
            .await?;

    if contact_id.is_some() {
        sqlx::query(direction.update_statement())
            .bind(occurred_at)
            .bind(occurred_at)
            .bind(&address)
            .execute(&mut **transaction)
            .await?;
    } else {
        let contact_id =
            sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
                .bind(occurred_at)
                .execute(&mut **transaction)
                .await?
                .last_insert_rowid();

        sqlx::query(direction.insert_statement())
            .bind(&address)
            .bind(contact_id)
            .bind(occurred_at)
            .bind(occurred_at)
            .execute(&mut **transaction)
            .await?;
    }

    Ok(())
}

pub async fn record_inbound(
    transaction: &mut Transaction<'_, Sqlite>,
    from_addr: &str,
    from_name: Option<&str>,
    received_at: &str,
) -> sqlx::Result<()> {
    record_message(
        transaction,
        from_addr,
        received_at,
        MessageDirection::Inbound,
    )
    .await?;

    if let Some(from_name) = from_name.filter(|name| !name.trim().is_empty()) {
        sqlx::query(
            "UPDATE contact_addresses
             SET display_name = ?
             WHERE address = ?
               AND last_seen <= ?",
        )
        .bind(from_name)
        .bind(normalize_address(from_addr))
        .bind(received_at)
        .execute(&mut **transaction)
        .await?;
    }

    Ok(())
}

pub async fn record_outbound(
    transaction: &mut Transaction<'_, Sqlite>,
    to_addrs: &[&str],
    sent_at: &str,
) -> sqlx::Result<()> {
    for to_addr in to_addrs {
        record_message(transaction, to_addr, sent_at, MessageDirection::Outbound).await?;
    }

    Ok(())
}

pub async fn profile_for(pool: &SqlitePool, address: &str) -> sqlx::Result<Option<Profile>> {
    let address = normalize_address(address);
    let row: Option<(Option<String>, i64, String, String, i64)> = sqlx::query_as(
        "SELECT COALESCE(
                    contacts.display_name,
                    (
                        SELECT observed.display_name
                        FROM contact_addresses AS observed
                        WHERE observed.contact_id = requested.contact_id
                        ORDER BY observed.last_seen DESC, observed.address
                        LIMIT 1
                    )
                ),
                SUM(facts.messages_in),
                MIN(facts.first_seen),
                MAX(facts.last_seen),
                MAX(facts.outbound_ever)
         FROM contact_addresses AS requested
         JOIN contacts ON contacts.id = requested.contact_id
         JOIN contact_addresses AS facts ON facts.contact_id = requested.contact_id
         WHERE requested.address = ?
         GROUP BY requested.contact_id, contacts.display_name",
    )
    .bind(address)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(display_name, messages_in, first_seen, last_seen, outbound_ever)| Profile {
            display_name,
            messages_in,
            first_seen,
            last_seen,
            outbound_ever: outbound_ever != 0,
        },
    ))
}

/// What a merge did, or why it would not.
///
/// The refusal is a first-class outcome rather than an error, because it is a question for the
/// person rather than a fault: two people they told the núcleo opposite things about cannot become
/// one person without one of those instructions being thrown away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    /// Both contacts carry a standing decision and the two disagree.
    RefusedConflictingVerdicts {
        keep: String,
        absorb: String,
    },
}

/// The standing verdict recorded against one contact, read inside a transaction.
async fn verdict_of(
    transaction: &mut Transaction<'_, Sqlite>,
    contact_id: i64,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT verdict FROM contact_overrides WHERE contact_id = ?")
        .bind(contact_id)
        .fetch_optional(&mut **transaction)
        .await
        .map(Option::flatten)
}

/// Joins two contacts into one, carrying the standing decision across.
///
/// The addresses move and the counters do not, which is what makes the join a pointer change and
/// splitting it again exact. What is new here is `contact_overrides`, which the address move alone
/// does not touch — and a pin lives on the CONTACT, so ignoring it had two failure modes with no
/// error between them: the absorbed person's decision became an unreadable orphan row, and their
/// mail silently started obeying the kept person's decision instead.
///
/// So the rule is that no merge discards an instruction. One decision moves to the survivor, two
/// identical ones collapse, and two that disagree refuse the merge outright. Picking a winner there
/// would be the system deciding something it was told twice and told differently — the same guess
/// the whole propose-don't-decide design exists to avoid.
async fn merge_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    keep_id: i64,
    absorb_id: i64,
    at: &str,
) -> sqlx::Result<MergeOutcome> {
    let keep_verdict = verdict_of(transaction, keep_id).await?;
    let absorb_verdict = verdict_of(transaction, absorb_id).await?;
    if let (Some(keep), Some(absorb)) = (&keep_verdict, &absorb_verdict)
        && keep != absorb
    {
        return Ok(MergeOutcome::RefusedConflictingVerdicts {
            keep: keep.clone(),
            absorb: absorb.clone(),
        });
    }

    sqlx::query(
        "UPDATE contact_addresses
         SET contact_id = ?,
             linked_by = 'human',
             linked_at = ?
         WHERE contact_id = ?",
    )
    .bind(keep_id)
    .bind(at)
    .bind(absorb_id)
    .execute(&mut **transaction)
    .await?;

    // The absorbed row goes either way: it is unreachable afterwards, since every verdict is looked
    // up through the addresses that now point at the survivor. When the survivor had no decision of
    // its own, the instruction moves rather than dying with the row it was written on.
    if let Some(absorb_verdict) = absorb_verdict {
        if keep_verdict.is_none() {
            sqlx::query(
                "INSERT INTO contact_overrides (contact_id, verdict, set_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT (contact_id) DO UPDATE SET verdict = excluded.verdict,
                                                        set_at  = excluded.set_at",
            )
            .bind(keep_id)
            .bind(&absorb_verdict)
            .bind(at)
            .execute(&mut **transaction)
            .await?;
        }
        sqlx::query("DELETE FROM contact_overrides WHERE contact_id = ?")
            .bind(absorb_id)
            .execute(&mut **transaction)
            .await?;
    }

    Ok(MergeOutcome::Merged)
}

/// Joins two contacts outside any proposal — test fixture only.
///
/// Production merges go through `approve_merge`, which does this and the proposal's status in ONE
/// transaction. A standalone version is a second, non-atomic way to reach the same tables, so
/// `#[cfg(test)]` keeps it available to the tests that exercise the join directly and unavailable
/// to anything that would use it to merge without an approval.
#[cfg(test)]
pub async fn merge(pool: &SqlitePool, keep_id: i64, absorb_id: i64) -> sqlx::Result<MergeOutcome> {
    let at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let outcome = merge_in_transaction(&mut transaction, keep_id, absorb_id, &at).await?;
    // A refusal wrote nothing, but rolling back rather than committing is what makes that true no
    // matter what is added to the function above this line.
    if outcome != MergeOutcome::Merged {
        return Ok(outcome);
    }
    transaction.commit().await?;
    Ok(outcome)
}

/// Splits one address back out into a person of its own.
///
/// The new contact inherits the standing decision, which is the only reading that leaves mail
/// behaving the way it did a moment earlier: the address was governed by that verdict right up to
/// the split, and a split is a statement about identity, not about what should happen to their
/// mail. Dropping it would silently withdraw an instruction nobody withdrew — and the address the
/// person would look at to check is precisely the one that lost it.
///
/// The original contact keeps its own copy, because the addresses left behind are still governed
/// by it.
pub async fn unmerge(pool: &SqlitePool, address: &str) -> sqlx::Result<()> {
    let address = normalize_address(address);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;

    // Read BEFORE the address is repointed, or it resolves through the new contact and finds
    // nothing — the verdict would be lost by the very statement meant to preserve it.
    let inherited: Option<String> = sqlx::query_scalar(
        "SELECT overrides.verdict
           FROM contact_overrides AS overrides
           JOIN contact_addresses AS addresses
             ON addresses.contact_id = overrides.contact_id
          WHERE addresses.address = ?",
    )
    .bind(&address)
    .fetch_optional(&mut *transaction)
    .await?
    .flatten();

    let contact_id =
        sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
            .bind(&created_at)
            .execute(&mut *transaction)
            .await?
            .last_insert_rowid();

    sqlx::query(
        "UPDATE contact_addresses
         SET contact_id = ?,
             linked_by = 'implicit',
             linked_at = NULL
         WHERE address = ?",
    )
    .bind(contact_id)
    .bind(&address)
    .execute(&mut *transaction)
    .await?;

    if let Some(verdict) = inherited {
        sqlx::query("INSERT INTO contact_overrides (contact_id, verdict, set_at) VALUES (?, ?, ?)")
            .bind(contact_id)
            .bind(verdict)
            .bind(&created_at)
            .execute(&mut *transaction)
            .await?;
    }

    transaction.commit().await
}

/// What setting a sender's standing verdict did.
///
/// `UnknownAddress` is not a failure and not a 500: it is what happens when someone pins an address
/// the núcleo has never received mail from. A contact exists because a message arrived, so there is
/// nothing to attach the decision to — and inventing a contact here would let a typo in an address
/// create a permanent, invisible row that never matches anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictOutcome {
    Applied,
    UnknownAddress,
}

/// Records — or clears — the standing decision about a sender.
///
/// The override lives on the CONTACT, not on the address, which is what makes it survive the same
/// person writing from a second address that later gets merged in. `verdict: None` clears it, and
/// clearing an address that had none is `Applied`: the caller asked for "no standing decision here",
/// and that is the state afterwards either way.
///
/// The verdict string is not validated here on purpose — `priority::is_known_verdict` owns that
/// question, and the endpoint asks it before this is reached. Splitting it that way keeps the
/// policy's vocabulary in the policy rather than half here and half there.
pub async fn set_verdict(
    pool: &SqlitePool,
    address: &str,
    verdict: Option<&str>,
) -> sqlx::Result<VerdictOutcome> {
    let address = normalize_address(address);
    let contact_id: Option<i64> =
        sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
            .bind(&address)
            .fetch_optional(pool)
            .await?;
    let Some(contact_id) = contact_id else {
        return Ok(VerdictOutcome::UnknownAddress);
    };

    match verdict {
        Some(verdict) => {
            sqlx::query(
                "INSERT INTO contact_overrides (contact_id, verdict, set_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT (contact_id) DO UPDATE SET verdict = excluded.verdict,
                                                        set_at  = excluded.set_at",
            )
            .bind(contact_id)
            .bind(verdict)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM contact_overrides WHERE contact_id = ?")
                .bind(contact_id)
                .execute(pool)
                .await?;
        }
    }

    Ok(VerdictOutcome::Applied)
}

/// One correspondent, as the daemon has come to know them.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Correspondent {
    pub address: String,
    /// Which person this address currently belongs to. Two rows sharing one id are two addresses a
    /// human said were the same person, which is the only way this list shows a merge happened.
    pub contact_id: i64,
    /// `human` when a person approved joining this address to its contact, `implicit` otherwise.
    /// It is what decides whether splitting it back out is offered.
    pub linked_by: String,
    pub display_name: Option<String>,
    pub messages_in: i64,
    /// Whether you have ever written to them. It is what `priority.rs` uses to decide a stranger
    /// from someone you know, so it is worth seeing next to the counts.
    pub outbound_ever: i64,
    pub first_seen: String,
    pub last_seen: String,
    /// The standing decision about them — `pin`, `mute`, or none.
    pub verdict: Option<String>,
}

/// Who writes to you, busiest first.
///
/// One row per address rather than per contact, deliberately: an address is what a message actually
/// carries and what a standing decision is looked up by, and merging two addresses into one person
/// is a thing the daemon proposes rather than does. Reporting the merged view would be reporting a
/// judgement that has not been made.
pub async fn roster(pool: &SqlitePool, limit: i64) -> sqlx::Result<Vec<Correspondent>> {
    sqlx::query_as(
        "SELECT addresses.address,
                addresses.contact_id,
                addresses.linked_by,
                addresses.display_name,
                addresses.messages_in,
                addresses.outbound_ever,
                addresses.first_seen,
                addresses.last_seen,
                overrides.verdict
           FROM contact_addresses AS addresses
           LEFT JOIN contact_overrides AS overrides
                  ON overrides.contact_id = addresses.contact_id
          ORDER BY addresses.messages_in DESC, addresses.last_seen DESC, addresses.address
          LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The standing verdict for whoever writes from this address, if anyone set one.
///
/// Resolved through the contact rather than the address for the same reason it is stored there: two
/// addresses merged into one person share one decision, and reading it per-address would report the
/// pin only for whichever address happened to be pinned.
pub async fn verdict_for(pool: &SqlitePool, address: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT overrides.verdict
           FROM contact_overrides AS overrides
           JOIN contact_addresses AS addresses
             ON addresses.contact_id = overrides.contact_id
          WHERE addresses.address = ?",
    )
    .bind(normalize_address(address))
    .fetch_optional(pool)
    .await
}

#[derive(Deserialize)]
struct ContactMergeInput {
    keep_id: i64,
    absorb_id: i64,
}

fn ordered_contact_pair(first_id: i64, second_id: i64) -> Option<(i64, i64)> {
    match first_id.cmp(&second_id) {
        std::cmp::Ordering::Less => Some((first_id, second_id)),
        std::cmp::Ordering::Greater => Some((second_id, first_id)),
        std::cmp::Ordering::Equal => None,
    }
}

fn contact_merge_pair(tool_input: &str) -> sqlx::Result<(i64, i64)> {
    let input: ContactMergeInput =
        serde_json::from_str(tool_input).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    ordered_contact_pair(input.keep_id, input.absorb_id).ok_or_else(|| {
        sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a contact merge must contain two different contact ids",
        )))
    })
}

pub async fn propose_merges(pool: &SqlitePool) -> sqlx::Result<Vec<i64>> {
    let address_rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT contact_id, display_name, outbound_ever
         FROM contact_addresses
         WHERE display_name IS NOT NULL
           AND TRIM(display_name) <> ''
         ORDER BY contact_id, address",
    )
    .fetch_all(pool)
    .await?;

    let mut people_by_name: BTreeMap<String, BTreeMap<i64, bool>> = BTreeMap::new();
    for (contact_id, display_name, outbound_ever) in address_rows {
        let normalized_name = display_name.trim().to_lowercase();
        let outbound = outbound_ever == 1;
        people_by_name
            .entry(normalized_name)
            .or_default()
            .entry(contact_id)
            .and_modify(|known_outbound| *known_outbound |= outbound)
            .or_insert(outbound);
    }

    let mut candidates = BTreeMap::new();
    for (normalized_name, people) in people_by_name {
        let people: Vec<_> = people.into_iter().collect();
        for (index, (lower_id, lower_outbound)) in people.iter().enumerate() {
            for (higher_id, higher_outbound) in &people[index + 1..] {
                if *lower_outbound || *higher_outbound {
                    candidates
                        .entry((*lower_id, *higher_id))
                        .or_insert_with(|| normalized_name.clone());
                }
            }
        }
    }

    let rejected_pairs: Vec<(i64, i64)> =
        sqlx::query_as("SELECT lower_id, higher_id FROM contact_merge_rejections")
            .fetch_all(pool)
            .await?;
    let rejected_pairs: BTreeSet<_> = rejected_pairs
        .into_iter()
        .filter_map(|(first_id, second_id)| ordered_contact_pair(first_id, second_id))
        .collect();

    let pending_inputs: Vec<String> = sqlx::query_scalar(
        "SELECT tool_input
         FROM proposals
         WHERE kind = 'contact-merge'
           AND status = 'pending'
           AND tool_input IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut pending_pairs: BTreeSet<_> = pending_inputs
        .iter()
        .filter_map(|tool_input| contact_merge_pair(tool_input).ok())
        .collect();

    let mut proposal_ids = Vec::new();
    for (pair, normalized_name) in candidates {
        if rejected_pairs.contains(&pair) || pending_pairs.contains(&pair) {
            continue;
        }

        let reasoning = format!(
            "Contacts share the normalized display name {normalized_name:?}, and at least one has outbound correspondence"
        );
        let proposal_id =
            crate::proposals::create_contact_merge(pool, pair.0, pair.1, &reasoning).await?;
        proposal_ids.push(proposal_id);
        pending_pairs.insert(pair);
    }

    Ok(proposal_ids)
}

/// One side of a suggested merge, named the way a person can recognise.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeSide {
    pub contact_id: i64,
    /// Every address currently under this contact.
    pub addresses: Vec<String>,
    pub display_name: Option<String>,
    pub messages_in: i64,
    /// The standing decision, so the conflict that would refuse the merge is visible BEFORE the
    /// person presses the button rather than as a 409 afterwards.
    pub verdict: Option<String>,
}

/// A pending suggestion that two contacts are one person.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeSuggestion {
    pub proposal_id: i64,
    pub reasoning: String,
    pub created_at: String,
    /// The contact that survives — the lower id, which is the order the pair is keyed on.
    pub keep: MergeSide,
    pub absorb: MergeSide,
}

async fn merge_side(pool: &SqlitePool, contact_id: i64) -> sqlx::Result<MergeSide> {
    let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT address, display_name, messages_in
           FROM contact_addresses
          WHERE contact_id = ?
          ORDER BY messages_in DESC, address",
    )
    .bind(contact_id)
    .fetch_all(pool)
    .await?;
    let verdict: Option<String> =
        sqlx::query_scalar("SELECT verdict FROM contact_overrides WHERE contact_id = ?")
            .bind(contact_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    Ok(MergeSide {
        contact_id,
        // Whichever address the person actually writes from carries the name worth showing, which
        // is why the busiest one is first and its name is the one taken.
        display_name: rows.iter().find_map(|row| row.1.clone()),
        messages_in: rows.iter().map(|row| row.2).sum(),
        addresses: rows.into_iter().map(|row| row.0).collect(),
        verdict,
    })
}

/// Every merge the heuristic has suggested and nobody has answered.
///
/// Kept out of `proposals::list_pending`, which stays action approvals only. The two kinds share a
/// table and share nothing else: an action approval is a paused run waiting for a signature and
/// carries a run, a session and a worktree; this is a question about a mailbox and carries a pair of
/// contact ids. One list rendering both would have to draw a card that is mostly absent fields.
///
/// The contact ids are resolved to addresses here because a person cannot answer "are 4 and 7 the
/// same person" — the addresses ARE the question.
pub async fn pending_merges(pool: &SqlitePool) -> sqlx::Result<Vec<MergeSuggestion>> {
    let rows: Vec<(i64, Option<String>, String, String)> = sqlx::query_as(
        "SELECT id, reasoning, tool_input, created_at
           FROM proposals
          WHERE kind = 'contact-merge'
            AND status = 'pending'
            AND tool_input IS NOT NULL
          ORDER BY id ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut suggestions = Vec::with_capacity(rows.len());
    for (proposal_id, reasoning, tool_input, created_at) in rows {
        // A row whose `tool_input` no longer parses is skipped rather than failing the list: it
        // would otherwise take every other suggestion down with it, and the one thing worse than an
        // unanswerable question is a screen that cannot show the answerable ones either.
        let Ok((keep_id, absorb_id)) = contact_merge_pair(&tool_input) else {
            tracing::warn!(
                proposal_id,
                "skipping a contact merge with unreadable input"
            );
            continue;
        };
        suggestions.push(MergeSuggestion {
            proposal_id,
            reasoning: reasoning.unwrap_or_default(),
            created_at,
            keep: merge_side(pool, keep_id).await?,
            absorb: merge_side(pool, absorb_id).await?,
        });
    }
    Ok(suggestions)
}

/// Why a merge decision could not be taken.
///
/// There is deliberately no `NotFound`: the guarded SELECT below matches on "this proposal, of this
/// kind, still pending", so a missing row and an already-decided one are indistinguishable here and
/// mean the same thing to the caller. The endpoint separates them by reading the proposal first,
/// which is where a 404 can honestly be told from a 409.
#[derive(Debug)]
pub enum DecisionError {
    NotPending,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        // `RowNotFound` reaches here only from the guarded SELECT below, whose WHERE clause is
        // "this proposal, of this kind, still pending" — so it is the answer to a decision arriving
        // after another one already landed, not a database fault.
        match error {
            sqlx::Error::RowNotFound => Self::NotPending,
            other => Self::Db(other),
        }
    }
}

/// Carries out a merge a person approved.
///
/// One transaction for the join and the proposal's status, the way `reject_merge` already does it
/// for the rejection and its memory. Split across two, a crash between them leaves either a merge
/// nobody approved or an approval that merged nothing, and only one of those is visible afterwards.
///
/// A refusal commits nothing and leaves the proposal PENDING on purpose: the conflicting decisions
/// are something the person can resolve — withdraw one of the two pins — and then approve the same
/// proposal. Marking it decided would make them wait for the heuristic to ask again.
pub async fn approve_merge(
    pool: &SqlitePool,
    proposal_id: i64,
) -> Result<MergeOutcome, DecisionError> {
    let at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await.map_err(DecisionError::Db)?;
    let tool_input: String = sqlx::query_scalar(
        "SELECT tool_input
         FROM proposals
         WHERE id = ?
           AND kind = 'contact-merge'
           AND status = 'pending'
           AND tool_input IS NOT NULL",
    )
    .bind(proposal_id)
    .fetch_one(&mut *transaction)
    .await?;
    // `keep` is the lower id and `absorb` the higher, which is the order `propose_merges` writes and
    // `contact_merge_rejections` keys on. Which of two contacts survives is arbitrary — they are the
    // same person — so a stable rule beats a meaningful-looking one.
    let (keep_id, absorb_id) = contact_merge_pair(&tool_input)?;

    let outcome = merge_in_transaction(&mut transaction, keep_id, absorb_id, &at).await?;
    if outcome != MergeOutcome::Merged {
        return Ok(outcome);
    }

    if !crate::proposals::transition_in_transaction(
        &mut transaction,
        proposal_id,
        "approved",
        "approved by user",
        &at,
    )
    .await?
    {
        // The compare-and-set found the proposal no longer pending, so another decision won the
        // race. Returning without committing discards this merge rather than applying one the
        // record will say was never approved.
        return Err(DecisionError::NotPending);
    }

    transaction.commit().await.map_err(DecisionError::Db)?;
    Ok(outcome)
}

pub async fn reject_merge(pool: &SqlitePool, proposal_id: i64) -> sqlx::Result<()> {
    let rejected_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let tool_input: String = sqlx::query_scalar(
        "SELECT tool_input
         FROM proposals
         WHERE id = ?
           AND kind = 'contact-merge'
           AND status = 'pending'
           AND tool_input IS NOT NULL",
    )
    .bind(proposal_id)
    .fetch_one(&mut *transaction)
    .await?;
    let (lower_id, higher_id) = contact_merge_pair(&tool_input)?;

    if !crate::proposals::transition_in_transaction(
        &mut transaction,
        proposal_id,
        "rejected",
        "rejected by user",
        &rejected_at,
    )
    .await?
    {
        return Err(sqlx::Error::RowNotFound);
    }

    sqlx::query(
        "INSERT INTO contact_merge_rejections (lower_id, higher_id, rejected_at)
         VALUES (?, ?, ?)
         ON CONFLICT (lower_id, higher_id)
         DO UPDATE SET rejected_at = excluded.rejected_at",
    )
    .bind(lower_id)
    .bind(higher_id)
    .bind(&rejected_at)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    type ContactAddressRow = (
        String,
        i64,
        String,
        String,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
    );

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn all_contact_address_rows(pool: &SqlitePool) -> Vec<ContactAddressRow> {
        sqlx::query_as(
            "SELECT address, contact_id, first_seen, last_seen, messages_in, outbound_ever,
                    linked_by, linked_at, display_name
             FROM contact_addresses
             ORDER BY address",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[test]
    fn uma_virgula_num_nome_nao_separa_dois_destinatarios() {
        let normalized =
            split_address_list(r#""Silva, Maria" <maria@example.com>, oncall@example.com"#)
                .into_iter()
                .map(normalize_address)
                .collect::<Vec<_>>();

        assert_eq!(normalized, vec!["maria@example.com", "oncall@example.com"]);
    }

    #[test]
    fn uma_virgula_dentro_dos_angulares_nao_separa() {
        let recipients = split_address_list("Maria <maria,alias@example.com>");

        assert_eq!(recipients.len(), 1);
    }

    #[test]
    fn uma_aspa_escapada_nao_fecha_o_nome() {
        let recipients = split_address_list(r#""Silva\", Maria" <maria@example.com>"#);

        assert_eq!(recipients.len(), 1);
    }

    #[tokio::test]
    async fn os_factos_sobrevivem_ao_prune() {
        let pool = test_pool().await;
        let address = "alice@example.com";
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let received_at =
            (now - Duration::days(crate::triage::ROW_RETENTION_DAYS + 1)).to_rfc3339();

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO emails (
                 message_id, mailbox, uidvalidity, uid, from_addr, received_at, ingested_at,
                 triage_class, triaged_at, direction
             )
             VALUES (?, 'INBOX', 1, 1, ?, ?, ?, 'info', ?, 'inbound')",
        )
        .bind("<old-inbound@example.com>")
        .bind(address)
        .bind(&received_at)
        .bind(&received_at)
        .bind(&received_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        record_inbound(&mut transaction, address, None, &received_at)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let before = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the accumulated contact profile must exist before pruning");
        assert_eq!(before.messages_in, 1);
        assert_eq!(before.first_seen, received_at);
        assert!(!before.outbound_ever);

        let (_, rows_removed) = crate::triage::prune(&pool, 7, now).await.unwrap();
        assert_eq!(rows_removed, 1);
        let emails_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM emails WHERE message_id = '<old-inbound@example.com>'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(emails_left, 0);

        let after = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("pruning the source email must not prune accumulated contact facts");
        assert_eq!(after.messages_in, before.messages_in);
        assert_eq!(after.first_seen, before.first_seen);
        assert_eq!(after.outbound_ever, before.outbound_ever);
    }

    #[tokio::test]
    async fn o_nome_sobrevive_ao_prune() {
        let pool = test_pool().await;
        let address = "helena@example.com";
        let display_name = "Helena Costa";
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let received_at =
            (now - Duration::days(crate::triage::ROW_RETENTION_DAYS + 1)).to_rfc3339();

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO emails (
                 message_id, mailbox, uidvalidity, uid, from_addr, from_name, received_at,
                 ingested_at, triage_class, triaged_at, direction
             )
             VALUES (?, 'INBOX', 1, 2, ?, ?, ?, ?, 'info', ?, 'inbound')",
        )
        .bind("<old-named-inbound@example.com>")
        .bind(address)
        .bind(display_name)
        .bind(&received_at)
        .bind(&received_at)
        .bind(&received_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        record_inbound(&mut transaction, address, Some(display_name), &received_at)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let (_, rows_removed) = crate::triage::prune(&pool, 7, now).await.unwrap();
        assert_eq!(rows_removed, 1);
        let emails_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM emails WHERE message_id = '<old-named-inbound@example.com>'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(emails_left, 0);

        let profile = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("pruning the source email must not prune the accumulated display name");
        assert_eq!(profile.display_name.as_deref(), Some(display_name));
    }

    #[tokio::test]
    async fn o_nome_segue_a_mensagem_mais_recente() {
        let pool = test_pool().await;
        let address = "helena@example.com";
        let earlier = "2026-07-20T09:00:00+00:00";
        let later = "2026-07-24T09:00:00+00:00";
        let latest = "2026-07-26T09:00:00+00:00";
        let unnamed_latest = "2026-07-27T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, Some("Helena Costa"), later)
            .await
            .unwrap();
        record_inbound(
            &mut transaction,
            address,
            Some("helena (old client)"),
            earlier,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let after_out_of_order = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the named inbound messages must create a profile");
        assert_eq!(
            after_out_of_order.display_name.as_deref(),
            Some("Helena Costa")
        );

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, Some("Helena C."), latest)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_new_name = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the later named inbound message must preserve the profile");
        assert_eq!(after_new_name.display_name.as_deref(), Some("Helena C."));

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, None, unnamed_latest)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_unnamed = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("an unnamed inbound message must preserve the profile");
        assert_eq!(after_unnamed.display_name.as_deref(), Some("Helena C."));
    }

    #[tokio::test]
    async fn o_nome_escrito_por_uma_pessoa_ganha() {
        let pool = test_pool().await;
        let address = "helena@example.com";
        let received_at = "2026-07-24T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(
            &mut transaction,
            address,
            Some("helena (mail client)"),
            received_at,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let contact_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(address)
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("UPDATE contacts SET display_name = ? WHERE id = ?")
            .bind("Helena Costa")
            .bind(contact_id)
            .execute(&pool)
            .await
            .unwrap();

        let profile = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the human-named contact must have a profile");
        assert_eq!(profile.display_name.as_deref(), Some("Helena Costa"));
    }

    #[tokio::test]
    async fn a_acumulacao_e_monotonica() {
        let pool = test_pool().await;
        let address = "bob@example.com";
        let earlier = "2026-07-20T09:00:00+00:00";
        let outbound_between = "2026-07-22T09:00:00+00:00";
        let later = "2026-07-24T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, None, later)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_first = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the first inbound fact must create a profile");
        assert_eq!(after_first.messages_in, 1);
        assert_eq!(after_first.first_seen, later);
        assert_eq!(after_first.last_seen, later);
        assert!(!after_first.outbound_ever);

        let mut transaction = pool.begin().await.unwrap();
        record_outbound(&mut transaction, &[address], outbound_between)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_outbound = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the outbound fact must preserve the profile");
        assert_eq!(after_outbound.messages_in, 1);
        assert!(after_outbound.outbound_ever);

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, None, earlier)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_second = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the second inbound fact must preserve the profile");
        assert_eq!(after_second.messages_in, after_first.messages_in + 1);
        assert_eq!(after_second.messages_in, 2);
        assert_eq!(after_second.first_seen, earlier);
        assert_eq!(after_second.last_seen, later);
        assert!(after_second.outbound_ever);
    }

    #[tokio::test]
    async fn o_insert_e_a_acumulacao_sao_atomicos() {
        let pool = test_pool().await;
        let address = "carol@example.com";
        let received_at = "2026-07-29T10:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO emails (
                 message_id, mailbox, uidvalidity, uid, from_addr, received_at, ingested_at,
                 direction
             )
             VALUES (?, 'INBOX', 1, 2, ?, ?, ?, 'inbound')",
        )
        .bind("<rolled-back@example.com>")
        .bind(address)
        .bind(received_at)
        .bind(received_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        record_inbound(&mut transaction, address, None, received_at)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();

        let emails_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM emails WHERE message_id = '<rolled-back@example.com>'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let contacts_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts")
            .fetch_one(&pool)
            .await
            .unwrap();
        let addresses_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_addresses")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(emails_left, 0);
        assert_eq!(contacts_left, 0);
        assert_eq!(addresses_left, 0);
        assert!(profile_for(&pool, address).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn o_perfil_de_uma_pessoa_soma_os_enderecos() {
        let pool = test_pool().await;
        let keep_address = "ana@example.com";
        let absorbed_address = "ana@work.example";
        let earliest = "2026-07-20T09:00:00+00:00";
        let second = "2026-07-21T09:00:00+00:00";
        let third = "2026-07-22T09:00:00+00:00";
        let absorbed_inbound = "2026-07-23T09:00:00+00:00";
        let latest = "2026-07-24T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, None, earliest)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, None, second)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, None, third)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, None, absorbed_inbound)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], latest)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(keep_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        let absorb_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();

        merge(&pool, keep_id, absorb_id).await.unwrap();

        let profile_from_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must resolve to the merged profile");
        let profile_from_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the absorbed address must resolve to the merged profile");

        assert_eq!(profile_from_keep.messages_in, 4);
        assert_eq!(profile_from_keep.first_seen, earliest);
        assert_eq!(profile_from_keep.last_seen, latest);
        assert!(profile_from_keep.outbound_ever);
        assert_eq!(
            profile_from_absorbed.messages_in,
            profile_from_keep.messages_in
        );
        assert_eq!(
            profile_from_absorbed.first_seen,
            profile_from_keep.first_seen
        );
        assert_eq!(profile_from_absorbed.last_seen, profile_from_keep.last_seen);
        assert_eq!(
            profile_from_absorbed.outbound_ever,
            profile_from_keep.outbound_ever
        );

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());
    }

    async fn contact_id_of(pool: &SqlitePool, address: &str) -> i64 {
        sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
            .bind(address)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Two addresses, each already its own contact, so a merge has something to join.
    async fn two_contacts(pool: &SqlitePool) -> (String, i64, String, i64) {
        let first = "bruno@example.com";
        let second = "bruna@example.com";
        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, first, None, "2026-07-10T08:00:00+00:00")
            .await
            .unwrap();
        record_inbound(&mut transaction, second, None, "2026-07-11T08:00:00+00:00")
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let first_id = contact_id_of(pool, first).await;
        let second_id = contact_id_of(pool, second).await;
        assert_ne!(first_id, second_id);
        (first.to_owned(), first_id, second.to_owned(), second_id)
    }

    async fn pin(pool: &SqlitePool, address: &str, verdict: &str) {
        set_verdict(pool, address, Some(verdict)).await.unwrap();
    }

    /// A pin lives on the CONTACT, and the join only moves addresses. Left alone, the absorbed
    /// person's instruction became an unreadable orphan row while their mail silently started
    /// obeying the other person's decision — two failures with no error between them.
    #[tokio::test]
    async fn fundir_carrega_a_decisao_de_quem_e_absorvido() {
        let pool = test_pool().await;
        let (_keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        pin(&pool, &absorbed, "mute").await;

        assert_eq!(
            merge(&pool, keep_id, absorb_id).await.unwrap(),
            MergeOutcome::Merged
        );

        // The instruction moved rather than dying with the row it was written on: the address that
        // was muted is still muted, now through the surviving contact.
        assert_eq!(
            verdict_for(&pool, &absorbed).await.unwrap().as_deref(),
            Some("mute")
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_overrides")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            rows, 1,
            "the absorbed contact must not keep an unreachable override"
        );
    }

    #[tokio::test]
    async fn fundir_recusa_quando_as_duas_decisoes_se_contradizem() {
        let pool = test_pool().await;
        let (keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        pin(&pool, &keep, "pin").await;
        pin(&pool, &absorbed, "mute").await;

        // Picking a winner would be the system deciding something it was told twice and told
        // differently — the exact guess the propose-don't-decide design exists to avoid.
        assert_eq!(
            merge(&pool, keep_id, absorb_id).await.unwrap(),
            MergeOutcome::RefusedConflictingVerdicts {
                keep: "pin".into(),
                absorb: "mute".into(),
            },
        );
        // And it wrote nothing: both people keep their own decision and their own identity.
        assert_eq!(
            verdict_for(&pool, &keep).await.unwrap().as_deref(),
            Some("pin")
        );
        assert_eq!(
            verdict_for(&pool, &absorbed).await.unwrap().as_deref(),
            Some("mute")
        );
        let still_apart: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT contact_id) FROM contact_addresses")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(still_apart, 2);
    }

    /// Splitting is a statement about identity, not about what should happen to their mail. The
    /// address was governed by that verdict right up to the split, and the address someone would
    /// look at to check is precisely the one that would have lost it.
    #[tokio::test]
    async fn desfundir_nao_retira_a_decisao_do_endereco_separado() {
        let pool = test_pool().await;
        let (keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        pin(&pool, &keep, "pin").await;
        merge(&pool, keep_id, absorb_id).await.unwrap();
        assert_eq!(
            verdict_for(&pool, &absorbed).await.unwrap().as_deref(),
            Some("pin")
        );

        unmerge(&pool, &absorbed).await.unwrap();

        assert_eq!(
            verdict_for(&pool, &absorbed).await.unwrap().as_deref(),
            Some("pin"),
            "the split address must keep behaving the way it did a moment earlier",
        );
        assert_eq!(
            verdict_for(&pool, &keep).await.unwrap().as_deref(),
            Some("pin"),
            "and the addresses left behind are still governed by the original",
        );
    }

    /// The whole point of wiring the endpoint: approving used to answer 409 and merge nothing.
    #[tokio::test]
    async fn aprovar_uma_proposta_funde_e_decide_a_proposta() {
        let pool = test_pool().await;
        let (keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        let proposal_id =
            crate::proposals::create_contact_merge(&pool, keep_id, absorb_id, "same name")
                .await
                .unwrap();

        assert_eq!(
            approve_merge(&pool, proposal_id).await.unwrap(),
            MergeOutcome::Merged
        );

        assert_eq!(
            contact_id_of(&pool, &keep).await,
            contact_id_of(&pool, &absorbed).await
        );
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "approved");

        // One decision only: a second approval finds nothing pending rather than merging again.
        assert!(matches!(
            approve_merge(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        ));
    }

    /// A refusal leaves the proposal answerable: withdraw one of the two pins and approve the same
    /// suggestion. Marking it decided would make the person wait for the heuristic to ask again.
    #[tokio::test]
    async fn uma_recusa_por_contradicao_deixa_a_proposta_por_responder() {
        let pool = test_pool().await;
        let (keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        pin(&pool, &keep, "pin").await;
        pin(&pool, &absorbed, "mute").await;
        let proposal_id =
            crate::proposals::create_contact_merge(&pool, keep_id, absorb_id, "same name")
                .await
                .unwrap();

        assert!(matches!(
            approve_merge(&pool, proposal_id).await.unwrap(),
            MergeOutcome::RefusedConflictingVerdicts { .. }
        ));
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");

        // Withdrawing one of the two is all it takes, and the same suggestion then goes through.
        set_verdict(&pool, &absorbed, None).await.unwrap();
        assert_eq!(
            approve_merge(&pool, proposal_id).await.unwrap(),
            MergeOutcome::Merged
        );
    }

    /// The suggestion has to be answerable by a person, and nobody can answer "are 4 and 7 the
    /// same person" — the addresses are the question.
    #[tokio::test]
    async fn as_sugestoes_pendentes_trazem_os_enderecos_e_as_decisoes() {
        let pool = test_pool().await;
        let (keep, keep_id, absorbed, absorb_id) = two_contacts(&pool).await;
        pin(&pool, &absorbed, "pin").await;
        crate::proposals::create_contact_merge(&pool, keep_id, absorb_id, "same name")
            .await
            .unwrap();

        let pending = pending_merges(&pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].keep.addresses, vec![keep]);
        assert_eq!(pending[0].absorb.addresses, vec![absorbed]);
        // The verdicts travel so the conflict that would refuse the merge is visible BEFORE the
        // button is pressed, rather than as a 409 afterwards.
        assert_eq!(pending[0].absorb.verdict.as_deref(), Some("pin"));
        assert!(pending[0].keep.verdict.is_none());
    }

    #[tokio::test]
    async fn fundir_e_desfundir_e_exacto() {
        let pool = test_pool().await;
        let keep_address = "bruno@example.com";
        let absorbed_address = "bruna@example.com";
        let keep_first = "2026-07-10T08:00:00+00:00";
        let keep_last = "2026-07-12T08:00:00+00:00";
        let absorbed_first = "2026-07-11T08:00:00+00:00";
        let absorbed_last = "2026-07-15T08:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, None, keep_last)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, None, keep_first)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, None, absorbed_first)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], absorbed_last)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(keep_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        let absorb_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(keep_id, absorb_id);

        let keep_before: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_before: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();

        merge(&pool, keep_id, absorb_id).await.unwrap();

        let merged_from_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must resolve after merging");
        let merged_from_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the absorbed address must resolve after merging");
        assert_eq!(merged_from_keep.messages_in, 3);
        assert_eq!(merged_from_keep.first_seen, keep_first);
        assert_eq!(merged_from_keep.last_seen, absorbed_last);
        assert!(merged_from_keep.outbound_ever);
        assert_eq!(
            merged_from_absorbed.messages_in,
            merged_from_keep.messages_in
        );
        assert_eq!(merged_from_absorbed.first_seen, merged_from_keep.first_seen);
        assert_eq!(merged_from_absorbed.last_seen, merged_from_keep.last_seen);
        assert_eq!(
            merged_from_absorbed.outbound_ever,
            merged_from_keep.outbound_ever
        );

        let merged_contact_ids: (i64, i64) = sqlx::query_as(
            "SELECT kept.contact_id, absorbed.contact_id
             FROM contact_addresses AS kept
             JOIN contact_addresses AS absorbed
             WHERE kept.address = ? AND absorbed.address = ?",
        )
        .bind(keep_address)
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(merged_contact_ids, (keep_id, keep_id));

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());

        unmerge(&pool, absorbed_address).await.unwrap();

        let keep_after: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_after: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(keep_after, keep_before);
        assert_eq!(absorbed_after, absorbed_before);

        let separated_contact_ids: (i64, i64) = sqlx::query_as(
            "SELECT kept.contact_id, absorbed.contact_id
             FROM contact_addresses AS kept
             JOIN contact_addresses AS absorbed
             WHERE kept.address = ? AND absorbed.address = ?",
        )
        .bind(keep_address)
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(separated_contact_ids.0, keep_id);
        assert_ne!(separated_contact_ids.0, separated_contact_ids.1);

        let separated_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must retain its original profile");
        assert_eq!(separated_keep.messages_in, 2);
        assert_eq!(separated_keep.first_seen, keep_first);
        assert_eq!(separated_keep.last_seen, keep_last);
        assert!(!separated_keep.outbound_ever);

        let separated_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the unmerged address must regain its original profile");
        assert_eq!(separated_absorbed.messages_in, 1);
        assert_eq!(separated_absorbed.first_seen, absorbed_first);
        assert_eq!(separated_absorbed.last_seen, absorbed_last);
        assert!(separated_absorbed.outbound_ever);
    }

    #[tokio::test]
    async fn fundir_nao_move_contadores() {
        let pool = test_pool().await;
        let keep_address = "carlos@example.com";
        let absorbed_address = "carla@example.com";
        let keep_first = "2026-07-01T07:00:00+00:00";
        let keep_last = "2026-07-03T07:00:00+00:00";
        let absorbed_first = "2026-07-02T07:00:00+00:00";
        let absorbed_last = "2026-07-05T07:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, None, keep_first)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, None, keep_last)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, None, absorbed_first)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], absorbed_last)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_before: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_before: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(keep_before.0, absorbed_before.0);

        merge(&pool, keep_before.0, absorbed_before.0)
            .await
            .unwrap();

        let keep_after: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_after: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(keep_after, keep_before);
        assert_eq!(absorbed_after.0, keep_before.0);
        assert_ne!(absorbed_after.0, absorbed_before.0);
        assert_eq!(absorbed_after.1, absorbed_before.1);
        assert_eq!(absorbed_after.2, absorbed_before.2);
        assert_eq!(absorbed_after.3, absorbed_before.3);
        assert_eq!(absorbed_after.4, absorbed_before.4);

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());
    }

    #[tokio::test]
    async fn a_heuristica_propoe_e_nao_funde() {
        let pool = test_pool().await;
        let first_address = "ana@example.com";
        let second_address = "ana@work.example";
        let received_at = "2026-07-20T09:00:00+00:00";
        let sent_at = "2026-07-21T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(
            &mut transaction,
            first_address,
            Some("Ana Silva"),
            received_at,
        )
        .await
        .unwrap();
        record_inbound(
            &mut transaction,
            second_address,
            Some(" ana silva "),
            received_at,
        )
        .await
        .unwrap();
        record_outbound(&mut transaction, &[first_address], sent_at)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let before = all_contact_address_rows(&pool).await;
        assert_eq!(before.len(), 2);
        let mut original_contact_ids = before.iter().map(|row| row.1).collect::<Vec<_>>();
        original_contact_ids.sort_unstable();
        original_contact_ids.dedup();
        assert_eq!(
            original_contact_ids.len(),
            2,
            "the fixture must begin with two separate people"
        );

        let created = propose_merges(&pool).await.unwrap();

        assert_eq!(created.len(), 1);
        let proposal_id = created[0];
        let (kind, status, reasoning, tool_input): (String, String, String, Option<String>) =
            sqlx::query_as(
                "SELECT kind, status, reasoning, tool_input
                 FROM proposals
                 WHERE id = ?",
            )
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "contact-merge");
        assert_eq!(status, "pending");
        assert!(!reasoning.trim().is_empty());

        let tool_input: serde_json::Value =
            serde_json::from_str(tool_input.as_deref().expect("merge ids must be recorded"))
                .unwrap();
        let mut proposed_contact_ids = vec![
            tool_input["keep_id"]
                .as_i64()
                .expect("tool_input must carry keep_id"),
            tool_input["absorb_id"]
                .as_i64()
                .expect("tool_input must carry absorb_id"),
        ];
        proposed_contact_ids.sort_unstable();
        assert_eq!(proposed_contact_ids, original_contact_ids);

        let event: (Option<String>, String, String) = sqlx::query_as(
            "SELECT from_status, to_status, note
             FROM proposal_events
             WHERE proposal_id = ?",
        )
        .bind(proposal_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.0, None);
        assert_eq!(event.1, "pending");
        assert_eq!(event.2, "created");

        let after = all_contact_address_rows(&pool).await;
        assert_eq!(
            after, before,
            "the heuristic must leave every contact-address value unchanged"
        );
        let distinct_people: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT contact_id) FROM contact_addresses")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(distinct_people, 2);
    }

    #[tokio::test]
    async fn um_par_recusado_nao_volta() {
        let pool = test_pool().await;
        let first_address = "helena@example.com";
        let second_address = "helena@work.example";
        let received_at = "2026-07-20T09:00:00+00:00";
        let sent_at = "2026-07-21T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(
            &mut transaction,
            first_address,
            Some("Helena Costa"),
            received_at,
        )
        .await
        .unwrap();
        record_inbound(
            &mut transaction,
            second_address,
            Some("helena costa"),
            received_at,
        )
        .await
        .unwrap();
        record_outbound(&mut transaction, &[second_address], sent_at)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let rows = all_contact_address_rows(&pool).await;
        let mut pair = rows.iter().map(|row| row.1).collect::<Vec<_>>();
        pair.sort_unstable();
        pair.dedup();
        assert_eq!(pair.len(), 2);

        let first_pass = propose_merges(&pool).await.unwrap();
        assert_eq!(first_pass.len(), 1);
        let proposal_id = first_pass[0];

        reject_merge(&pool, proposal_id).await.unwrap();

        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(status, "pending");
        assert_eq!(status, "rejected");

        let rejection: (i64, i64, String) = sqlx::query_as(
            "SELECT lower_id, higher_id, rejected_at
             FROM contact_merge_rejections
             WHERE lower_id = ? AND higher_id = ?",
        )
        .bind(pair[0])
        .bind(pair[1])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((rejection.0, rejection.1), (pair[0], pair[1]));
        assert!(!rejection.2.is_empty());

        let second_pass = propose_merges(&pool).await.unwrap();
        assert!(
            second_pass.is_empty(),
            "a rejected pair must not produce another proposal"
        );
        let proposal_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposals WHERE kind = 'contact-merge'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(proposal_count, 1);
    }

    #[tokio::test]
    async fn a_heuristica_exige_evidencia_de_conhecimento() {
        let pool = test_pool().await;
        let first_address = "info@first.example";
        let second_address = "info@second.example";
        let received_at = "2026-07-20T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(
            &mut transaction,
            first_address,
            Some("Support Team"),
            received_at,
        )
        .await
        .unwrap();
        record_inbound(
            &mut transaction,
            second_address,
            Some(" support team "),
            received_at,
        )
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        let rows = all_contact_address_rows(&pool).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.5 == 0));
        assert_ne!(rows[0].1, rows[1].1);

        let created = propose_merges(&pool).await.unwrap();

        assert!(
            created.is_empty(),
            "a shared display name without outbound correspondence is insufficient"
        );
        let proposal_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposals WHERE kind = 'contact-merge'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(proposal_count, 0);
    }
}
