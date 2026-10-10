//! The project map (`/projects/{id}/map/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/map", get(get_project_map))
        // Which specs a project has, so the extraction button offers a list and not a text box.
        // Beside `map` because it answers about the same tree, read the same way.
        .route("/projects/{id}/map/specs", get(get_project_map_specs))
        // One file's own declarations — the step below the file, fetched only when somebody opens
        // one. Beside `map` because it answers about the same tree; separate from it because
        // folding it in would multiply the largest answer this daemon sends by the size of the
        // project, to carry a level nobody has asked for yet.
        .route("/projects/{id}/map/items", get(get_project_map_items))
        // The intention layer, beside the structure layer it will one day be joined to. A POST and
        // deliberately in no table in `auth.rs`: reading a map costs nothing and a read-only key
        // buys it, while extracting spends a model — which is not something that key ever bought.
        .route("/projects/{id}/map/extract", post(post_project_map_extract))
        // The pile and the answer to one line of it. The GET is a read and is in
        // `READ_ONLY_ROUTES`; the POST is in no table, beside `extract` above and for a sharper
        // reason — approving is the owner's stamp, and it is what puts a line in the map.
        //
        // One id in the path and no bulk form. §4 says the list is approved line by line, and a
        // route that took the whole list would be the thousand-line plan again, wearing a smaller
        // shape.
        .route(
            "/projects/{id}/map/decisions",
            get(get_project_map_decisions),
        )
        .route(
            "/projects/{id}/map/decisions/{decision}",
            post(post_project_map_decision),
        )
        // §6.2's pile, on a door of its own rather than a query parameter on `map`. *"Sempre
        // acessível"* is the requirement, and a pile you reach only by knowing which parameter to
        // add is not — §13 names the triager silencing what it should have shown a real residual
        // risk, and this route IS the mitigation. A GET, and in `READ_ONLY_ROUTES` beside `map` and
        // `map/decisions` on their argument exactly: sentences lifted out of a document already
        // sitting in a folder this reader can `cat` whole.
        .route("/projects/{id}/map/silenced", get(get_project_map_silenced))
        // §14's guard, and the only route in this pillar that reads git's HISTORY rather than its
        // present. A GET, in `READ_ONLY_ROUTES`, and reached from ONE place — a row of §5.1's
        // *declarado, sem código* — because the question it answers only means anything there. It
        // costs a walk of the history per call, which is why it is not folded into `map`: every
        // decision in that pile would pay for it and almost none of them would be asked.
        .route("/projects/{id}/map/orphan", get(get_project_map_orphan))
        // The owner's verdict on one decision (§5.2). In no table in `auth.rs` for the sharpest
        // reason of the three POSTs here: §6 takes the green away from the model on purpose, and
        // §6.1 says handing that authority back through any other door turns the map into false
        // confidence with the authority of a traffic light. A read-only key writing one would be
        // that door.
        //
        // The id is in the body and not in the path, unlike `decisions/{decision}` above. The two
        // are different acts: that one answers a line that is in front of the owner right now and is
        // addressed by where it sits in the pile, while a stamp is a new row about a decision that
        // is already in the map — §9.2's *acrescenta uma linha, nunca substitui* — and it carries a
        // verdict and a note that have to travel in a body regardless.
        .route("/projects/{id}/map/stamps", post(post_project_map_stamp))
        // §14's other half: which files a decision's code IS, written down so that deleting
        // the `§` comment stops erasing the association. In NO table in `auth.rs`, beside
        // `stamps` and for a sharper version of its argument — a record is what the whole map
        // anchors to, so a key that could write one could point every decision at whatever it
        // liked and the map would report it as the owner's own choice.
        .route("/projects/{id}/map/anchors", post(post_project_map_anchors))
        // The model's second entrance (§6), and in no table in `auth.rs` for `extract`'s reason
        // doubled: it spends a model per decision. No id anywhere on it, and that is §10 rather
        // than an omission — the triager's scope is everything nobody has stamped, and a caller
        // that could name the decisions could name the ones whose answer it liked.
        .route("/projects/{id}/map/triage", post(post_project_map_triage))
}

/// Which documents this project has, from the table first and the folder second.
///
/// **The union, and the order of the two words is the whole argument.** `map::intent::specs_in`
/// answers *what is in `.ai/specs` right now*; `map::store::slugs` answers *what this project has
/// ever extracted decisions from*. Only the second is durable — spec files are gitignored working
/// material that gets archived, renamed and reorganised, and the slug is the FILENAME, so a rename
/// silently retires every slug the approved rows still carry.
///
/// What the list is for is [`crate::map::join::evidence`]'s one refusal: a citation whose candidate
/// names a DIFFERENT document of this project is evidence against, and is skipped rather than
/// counted. A list that lost a slug therefore does not fail loudly — it quietly stops being able to
/// tell two documents apart, every anchor degrades to ambiguous, and nothing on screen says why.
/// That is the silent wrong answer this feature exists to refuse, arriving through a file rename.
///
/// **The folder still contributes**, because a spec sitting there with no decisions yet is a real
/// document of this project and a citation naming it must still be refused for the others. Union
/// never loses a slug; either source alone does.
///
/// Sorted and deduplicated so two reads of an unchanged project cannot produce two different lists,
/// which `map::join::join`'s determinism rests on.
pub(super) async fn document_slugs(
    state: &AppState,
    id: &str,
    on_disk: Vec<String>,
) -> Result<Vec<String>, StatusCode> {
    let mut all: std::collections::BTreeSet<String> = on_disk.into_iter().collect();
    all.extend(crate::map::store::slugs(&state.pool, id).await.map_err(
        |error| {
            tracing::warn!(%error, project_id = %id, "reading this project's document slugs failed");
            StatusCode::INTERNAL_SERVER_ERROR
        },
    )?);
    Ok(all.into_iter().collect())
}

/// The structure layer and the junction over it, in one answer.
///
/// **Flattened, so this is additive on the wire.** Every field the shell already reads — `modules`,
/// `imports`, `unread`, `foreign` — stays exactly where it was and `junction` is simply new beside
/// them. Nesting the structure under a key to make room would be a rewrite wearing the word
/// "extension", and would break a window nobody asked to change.
#[derive(Serialize)]
pub(super) struct MapAnswer {
    #[serde(flatten)]
    pub(super) structure: crate::map::Structure,
    /// §16.4's L0: the boundary between the two sides of this product, read from both ends.
    ///
    /// **On this answer and not on a route of its own, unlike `map/items`, and the difference is
    /// which question it belongs to.** Items are asked about one file once somebody opens it, so
    /// their cost lands on a click. The seam is the top of the picture and is on screen the moment
    /// the map is, so a route of its own would be a second request fired every time — and it would
    /// walk the tree again to learn what `structure` above already knows. It is computed from
    /// exactly those modules for the same reason: one definition of *which files are this
    /// project*.
    pub(super) seam: crate::map::seam::Seam,
    pub(super) junction: crate::map::join::Junction,
    /// Where each decision stands, by `decision_id`.
    ///
    /// **A map and never a parallel array.** Two arrays that must stay index-aligned is a bug
    /// waiting for the first re-sort, and the re-sort duly arrived: `junction.decisions` leaves
    /// [`crate::map::join::join`] in `spec_slug, ordinal, id` and leaves this handler in §10's
    /// recency order. Keyed by id, the client survived that; keyed by position it would now be
    /// drawing one decision's verdict against another's text, and nothing on the screen would look
    /// wrong.
    pub(super) standings: std::collections::BTreeMap<i64, crate::map::stamp::Standing>,
    /// §5.3's header, tallied from exactly the standings above.
    pub(super) stamps: crate::map::stamp::StampCounts,
    /// What the triager said about each decision, by `decision_id` — and only where that is still
    /// an answer about this map.
    ///
    /// **Two filters stand between [`crate::map::store::judgements`] and this field, and both are
    /// required for §5.3's numbers to add up.** [`crate::map::triage::reconcile`] argues them in
    /// full; the short of it is that a judgement about a decision somebody has since stamped
    /// describes a decision that has left triage's scope (§10), and a judgement whose
    /// `inputs_digest` no longer matches describes code that has since moved. Counting either
    /// reports something that is not true now — the first as a decision tallied in `settled` **and**
    /// in `flagged`, the second as a model's opinion of a file it never saw.
    ///
    /// **A map and never a parallel array**, for [`MapAnswer::standings`]'s reason: this list gets
    /// re-sorted into §10's order after the join.
    ///
    /// **The staleness test is three-valued, and the third value is why a judgement is kept rather
    /// than dropped when git will not answer.** A judgement whose digest cannot be COMPUTED — the
    /// decision's anchors came back [`crate::map::stamp::Anchors::Failed`] — is neither current nor
    /// expired, so it arrives here [`crate::map::triage::Held::checked`] `false` and counts in
    /// whichever pile it was already in. Dropping it was the first shape of this field and it was
    /// wrong at the scale of the whole project: every stored judgement is written against a
    /// `Computed(...)` reading, so one git hiccup made all of them stale at once and the map
    /// reported a three-hundred-row backlog as *nunca vista* — a fact about this daemon presented
    /// as a fact about the project. A flag we could not re-verify is still a flag; a flag we
    /// discarded becomes a claim that nobody ever looked. [`MapAnswer::git_would_not_answer`] is on
    /// the same answer and says why the flags are unchecked.
    ///
    /// A judgement that WAS tested and failed is genuinely dropped, and the decision reads
    /// untriaged, which is what it is: §10 sends the triager back over it.
    pub(super) triage: std::collections::BTreeMap<i64, crate::map::triage::Held>,
    /// §5.3's `J à tua espera` and `K nunca vistas`, tallied in the same pass that built `triage`.
    ///
    /// **Beside [`MapAnswer::stamps`] rather than inside it**, which is
    /// [`crate::map::triage::TriageCounts`]'s own argument: `stamps` is the denominator these numbers
    /// are checked against, and one function producing both sides of `flagged + silenced + untriaged
    /// == never` could not be checked by anything.
    pub(super) triage_counts: crate::map::triage::TriageCounts,
    /// **One fact about this whole reading, and never a fact about any single decision.**
    ///
    /// `map::stamp::digest` is all-or-nothing on purpose — one record it cannot parse takes the
    /// entire call to a failure, because dropping an anchor would read as `gone`, a file that never
    /// went anywhere. Correct for one anchor set; combined with this route calling it **once** for
    /// the union of every decision's anchors, it means a git that will not answer takes every
    /// settled stamp in the project to [`crate::map::stamp::Lapse::Unreadable`] at the same instant.
    ///
    /// That blast radius is accepted — the alternative is up to 350 process spawns on a route the
    /// window calls every time the map opens — and this flag is the price of accepting it. The
    /// owner meets one sentence saying git would not answer, rather than a screen of identical rows
    /// saying it 350 times, which is a wall of noise nobody reads to the bottom of and is how the
    /// one real lapse underneath it goes unseen.
    ///
    /// **Named for git refusing, and it was renamed from `anchors_unreadable` to make one thing
    /// impossible: printing *try again* at somebody with nothing to retry.** This flag is
    /// [`crate::map::stamp::Anchors::Failed`] and nothing else. A project with no git repository at
    /// all — §11's ordinary case — leaves it `false`, because there is nothing wrong there and no
    /// retry that could help; what that project gets instead is
    /// [`crate::map::stamp::Watch::NoRepository`] on each of its greens, which is a permanent fact
    /// and reads as one. The older name covered both, and the sentence a shell would have written
    /// under it was false for exactly the project that could do least about it.
    ///
    /// A decision with no anchor paths is untouched by this either way: git was never asked about
    /// it, so its digest is `""` however git is faring, and it stays
    /// [`crate::map::stamp::Watch::NoAnchor`] rather than flapping every time this goes up.
    pub(super) git_would_not_answer: bool,
    /// §10's ordering — what `junction.decisions` is sorted by, and how far that sort can see.
    ///
    /// **Beside the list rather than folded into it, because the order is only half the answer.**
    /// A list carries the sequence and nothing about which part of it is a fact: on this repository
    /// 83 of the 112 anchor files fall inside the window and every decision behind the other 29
    /// ties, so a panel drawing this without [`crate::map::recency::Recency::window`] would present
    /// a mostly arbitrary tail with the same confidence as the head. §10 offers this ordering as
    /// *um facto do git*, and the honest way to keep that true is to say how far the git looked.
    pub(super) recency: crate::map::recency::Recency,
    /// When this project's triager last answered anything, or `null` if it never has.
    ///
    /// **The one thing `triage` and `triage_counts` cannot say, and a panel was reduced to guessing
    /// it.** Both of those describe what is true NOW: a run that flagged everything and whose
    /// answers have all since gone stale leaves `triage` empty and §6.2's pile empty, which is
    /// indistinguishable from a project nobody has ever pressed the button on. Those two are not the
    /// same fact — the first says the triager's work expired, the second says none was ever done —
    /// and the panel wrote *as far as this map can see, it has never run* because that was the
    /// strongest true sentence available to it. A hedge is what an honest surface produces when the
    /// daemon declines a question it could answer.
    ///
    /// **Deliberately unfiltered by staleness and by standing.** See
    /// [`crate::map::store::last_triaged`]: filtering either way would make `null` mean *nothing it
    /// said still stands*, which is a different sentence and one the numbers already carry.
    pub(super) last_triaged_at: Option<String>,
}

/// The project's whole graph: modules, imports, and what the approved decisions do or do not
/// anchor to.
///
/// Derived on every request and never stored — decision 1 of the spec. The walk and the join are
/// [`project_junction`], which the stamp route shares; the digest, the standings and §5.3's header
/// are this route's own.
///
/// **Three axes arrive beside one another and none is merged into the others.** §5 refuses to
/// flatten them — *"achatá-las numa só punha o triador e o dono a falar pela mesma boca"* — so
/// `junction` still says only what can be known without anybody looking, `standings` says what the
/// owner said and whether it is still true, and `triage` says what a model thought was worth their
/// eyes. A client is free to draw them together; it is not free to be handed one number that has
/// already decided how they combine. That is why `triage` is a map of verdicts and not a colour,
/// and why §6.1 spends a paragraph on the one combination that may never be drawn: *silenciado* is
/// a claim about the triager, and a green is the owner's.
///
/// **What this now answers that it did not: which decisions nothing implements.** The structure
/// alone cannot say it — it has never heard of a decision — and the pile alone cannot say it
/// either. Only the two read against each other can, and doing that by eye across a thousand-line
/// plan is precisely what the owner cannot do; decision 3 says the nodes that matter are the ones
/// that do not match, so a route that returned only the halves returned everything except the
/// product.
///
/// **The join is computed here and never by whoever draws it** — §9.3 makes `core/` the single
/// owner of that logic. A shell redoing it in TypeScript would be a second implementation, free to
/// drift from this one, and the drift would surface as a screen quietly confident about the wrong
/// nodes: the disease with better pixels. It also means `map_join`'s tests are tests of what ships,
/// rather than of a library the product bypasses.
///
/// **Nothing here filters on `approved_at`.** [`crate::map::store::approved`] already selects
/// `approved_at IS NOT NULL AND retired_at IS NULL`, and a second filter in this handler is the
/// one that goes stale — the argument [`crate::map::store::decide`] makes about checks that live in
/// handlers, and the failure `from_row` next door already had once.
///
/// A missing folder is a 404 and a failed query is a 500 — see [`project_junction`], which is where
/// both now happen.
pub(super) async fn get_project_map(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<MapAnswer>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;
    let (structure, mut junction) = project_junction(&state, &id, root.clone()).await?;

    // Re-reads the sources `structure` already opened, and that is the cheaper of the two options
    // rather than an oversight: holding every file of a thousand-file project in memory to save one
    // pass would trade a bounded read for an unbounded allocation on a route somebody is waiting
    // on. `spawn_blocking` because it is file IO in an async handler, the same reason the walk
    // above is one.
    let seam = {
        let root = root.clone();
        let modules = structure.modules.clone();
        tokio::task::spawn_blocking(move || crate::map::seam::seam(&root, &modules))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|error| {
                tracing::warn!(%error, project_id = %id, "reading the seam failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
    };

    // One `git ls-files` for the union of every decision's anchor paths, sliced per decision
    // afterwards, and the same reading `POST /map/triage` shows the model. See [`anchor_digests`]
    // for why it is one call and one function.
    let (readings, anchors) = anchor_digests(&root, &junction.decisions).await;

    // §10's order, and the SAME call `POST /map/triage` makes before its sweep. Two orderings would
    // be two answers to one sentence of the spec, and the one deciding which twenty decisions get a
    // model call spent on them would be the one nobody ever looked at.
    let walked = crate::map::recency::walk(&root).await;
    let recency = crate::map::recency::order(&mut junction.decisions, &walked);

    let stamped = crate::map::store::stamps(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "project map stamps failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let latest: std::collections::BTreeMap<i64, &crate::map::store::Stamp> = stamped
        .iter()
        .map(|stamp| (stamp.decision_id, stamp))
        .collect();

    // Read beside the judgements and never derived from them, because it answers the question they
    // cannot: `answered` is what still describes this map, and a run whose every answer has since
    // gone stale comes back from it empty — identical to a project nobody ever pressed the button
    // on. One `MAX` over the table tells those two apart, and it is the difference between a panel
    // saying *it has never run here* and a panel hedging.
    let last_triaged_at = crate::map::store::last_triaged(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the last triage run failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // The whole pile, stale rows and all, and the staleness decided below where the reading of the
    // repository is. `map::store::judgements` cannot decide it — see `Judged`, which argues why the
    // comparison belongs to whoever holds that reading, and what a store that filtered would have
    // cost §6.2's pile.
    let answered = crate::map::store::judgements(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the triage pile failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let mut stored: std::collections::BTreeMap<i64, crate::map::store::Judged> = answered
        .into_iter()
        .map(|judged| (judged.decision_id, judged))
        .collect();

    // One clock for the whole answer rather than one per decision. Two amber stamps on either side
    // of `NOTE_LIFETIME` must be judged against the same instant, or the header can report a total
    // that was never true at any single moment.
    let now = chrono::Utc::now();
    let mut standings = std::collections::BTreeMap::new();
    // The judgements that survived the staleness test, in the order the decisions come in.
    // `map::triage::reconcile` applies the other filter — the one about standing — and tallies what
    // is left in the same pass that builds the map this answer ships.
    let mut current = Vec::new();
    for anchored in &junction.decisions {
        let anchors = anchor_reading(&readings, anchored.decision_id);
        standings.insert(
            anchored.decision_id,
            crate::map::stamp::standing(
                latest.get(&anchored.decision_id).copied(),
                crate::map::stamp::Anchoring {
                    current: anchors,
                    named: anchored.modules.len(),
                    // `Declared` and not merely *something names this section*. §8 is unfixed, so
                    // this is `false` for every decision in this repository today, and that is the
                    // measurement rather than a defect — see `Watch::Guessed`.
                    declared: anchored.anchor == crate::map::join::Anchor::Declared,
                },
                now,
            ),
        );

        // The digest is computed only for decisions that actually have a judgement, which is what
        // keeps this loop's cost proportional to the pile rather than to the map: on day one there
        // are ~350 decisions and no judgements at all, and a hash per decision would be 350 of them
        // to compare against nothing. The evidence is the SAME `Evidence` `POST /map/triage`
        // builds — one derivation, or a judgement could read current on one route and stale on the
        // other with nothing having happened.
        if let Some(judged) = stored.remove(&anchored.decision_id) {
            // **Three answers and not two.** With `Anchors::Failed` there is no digest to compare
            // against — `inputs_digest` would hash the `git-failed` tag, which every stored
            // judgement mismatches by construction — so asking `is_current` here would not be a
            // staleness test at all, it would be a guaranteed *no* dressed as one. The judgement is
            // kept and marked instead, exactly as `POST /map/triage` skips such a decision rather
            // than triaging it under a digest that dies when git recovers.
            if matches!(anchors, crate::map::stamp::Anchors::Failed) {
                current.push(crate::map::triage::Held {
                    checked: false,
                    judged,
                });
                continue;
            }
            let digest = crate::map::triage::inputs_digest(crate::map::triage::Evidence {
                decision: anchored,
                anchors,
            });
            if crate::map::triage::is_current(&judged.inputs_digest, &digest) {
                current.push(crate::map::triage::Held {
                    checked: true,
                    judged,
                });
            }
        }
    }

    let stamps = crate::map::stamp::counts(&standings.values().cloned().collect::<Vec<_>>());
    // The map the panel draws and the two numbers above it, out of one pass over one reading. Two
    // calls — one to filter, one to tally — would be two chances to ship one set and count another,
    // and the screen would look entirely correct while doing it.
    let crate::map::triage::Reconciled {
        judgements: triage,
        counts: triage_counts,
    } = crate::map::triage::reconcile(&standings, &stamps, current);
    // The two header lines on that screen come from different places — this one from
    // `standings.len()`, the other from the join — and they can only agree if the loop above put
    // exactly one standing against every approved decision. A test asserts it too, because a
    // `debug_assert` says nothing in the build the owner runs.
    debug_assert_eq!(
        stamps.decisions, junction.counts.decisions,
        "one standing per approved decision, or the header disagrees with itself"
    );

    Ok(Json(MapAnswer {
        structure,
        seam,
        junction,
        standings,
        stamps,
        triage,
        triage_counts,
        git_would_not_answer: anchors == crate::map::stamp::Anchors::Failed,
        recency,
        last_triaged_at,
    }))
}

/// Every decision's anchor code as it stands now, from ONE `git ls-files` for the union of their
/// paths.
///
/// **One function and not a second walk, for the reason [`project_junction`] is one.** `GET /map`
/// asks this to say where a decision stands and `POST /map/triage` asks it to say what the triager
/// was shown and whether its last answer is still about that. Two derivations would be two answers
/// to one question, free to disagree — and the disagreement would surface as a judgement about
/// files the map does not think are anchors, or as a stamp expired against a set nobody stamped it
/// over.
///
/// **The union call, sliced per decision afterwards** — and the obvious later refactor is the
/// per-decision loop, so both reasons it is not written that way are here rather than left to be
/// rediscovered.
///
/// The first is cost. This repository has ~350 approved decisions and the map is read every time
/// the window opens, so a call each would be ~350 process spawns on a route somebody is waiting
/// on. The second is the ceiling: the union of this repository's paths measures 732 entries and
/// **24 224 characters of argv — 75 % of the 32 767 Windows allows** — so the chunking
/// `map::stamp::digest` already does is load-bearing here rather than decorative, and a project a
/// third larger than this one goes over it.
///
/// The union must produce exactly what the per-decision loop would have, or it is an optimisation
/// that changes answers. Two rules keep that true: the slice below is **by path**, never by the
/// order git happened to answer in, and a decision with no anchor paths gets `Some("")` without
/// consulting the call at all — which is what `digest` itself does with an empty list, and is why
/// a git failure leaves those decisions on `Watch::NoAnchor` instead of flapping to *unreadable*.
///
/// Answers the per-decision reading **and** the union call's own result. The second is not
/// derivable from the first: a project where every decision names no file has a map full of
/// `Computed("")` whatever git did, and *git would not answer* is a fact about the repository that
/// only the union call holds.
pub(super) async fn anchor_digests(
    root: &std::path::Path,
    decisions: &[crate::map::join::Anchored],
) -> (
    std::collections::BTreeMap<i64, crate::map::stamp::Anchors>,
    crate::map::stamp::Anchors,
) {
    // `watched` and not `modules`, which is the whole of §14's cure reaching the expiry rule: a
    // stamp has to keep watching the files somebody wrote down even after the comment that first
    // named them is gone. Watching `modules` alone would mean a deleted comment silently shrinks
    // the set a green expires against — the green stops watching the very file it was given over.
    let per_decision_watched: std::collections::BTreeMap<i64, Vec<String>> = decisions
        .iter()
        .map(|anchored| (anchored.decision_id, anchored.watched()))
        .collect();
    let mut union: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for paths in per_decision_watched.values() {
        union.extend(paths.iter().map(String::as_str));
    }
    let union: Vec<String> = union.into_iter().map(str::to_owned).collect();
    let anchors = crate::map::stamp::digest(root, &union).await;
    // Parsed once and not once per decision: 350 decisions against a 52 KB digest is 350 walks of
    // the same text for an answer that cannot change between them.
    let tracked = match &anchors {
        crate::map::stamp::Anchors::Computed(text) => Some(crate::map::stamp::parse(text)),
        _ => None,
    };

    let mut per_decision = std::collections::BTreeMap::new();
    for anchored in decisions {
        // A decision with no anchor paths is `Computed("")` whatever became of the git call, because
        // `digest` itself never asks git for an empty list — so a project with no repository leaves
        // these on `Watch::NoAnchor`, which is the fact the owner can act on, rather than on
        // `Watch::NoRepository`, which would be true about the folder and useless about the decision.
        let watched = per_decision_watched
            .get(&anchored.decision_id)
            .expect("every decision was given a watched set just above");
        let current = if watched.is_empty() {
            crate::map::stamp::Anchors::Computed(String::new())
        } else {
            match (&anchors, &tracked) {
                (crate::map::stamp::Anchors::Computed(_), Some(tracked)) => {
                    crate::map::stamp::Anchors::Computed(crate::map::stamp::canonical(
                        watched.iter().filter_map(|path| {
                            tracked
                                .get(path.as_str())
                                .map(|blob| (path.as_str(), blob.as_str()))
                        }),
                    ))
                }
                (crate::map::stamp::Anchors::NoRepository, _) => {
                    crate::map::stamp::Anchors::NoRepository
                }
                _ => crate::map::stamp::Anchors::Failed,
            }
        };
        per_decision.insert(anchored.decision_id, current);
    }
    (per_decision, anchors)
}

/// The anchor reading of one decision, or the value that claims nothing if it somehow has none.
///
/// Unreachable by construction — [`anchor_digests`] puts exactly one entry against every decision
/// of the slice it was handed, and both callers hand it `junction.decisions` — and written anyway,
/// because the alternative to a fallback here is a panic in a handler and the alternative to both
/// is dropping the decision, which would take it silently out of a header §5.3 requires to add up.
///
/// [`crate::map::stamp::Anchors::Failed`] is the one value that is safe to be wrong with. It means
/// *this daemon could not read the anchors*, which is exactly true if this were ever reached: a
/// stamp reads as unreadable rather than as green, and a judgement made against it goes stale the
/// moment git answers again. Both err towards asking, which is the direction §1 can afford.
pub(super) fn anchor_reading(
    readings: &std::collections::BTreeMap<i64, crate::map::stamp::Anchors>,
    decision_id: i64,
) -> &crate::map::stamp::Anchors {
    const UNREADABLE: crate::map::stamp::Anchors = crate::map::stamp::Anchors::Failed;
    readings.get(&decision_id).unwrap_or(&UNREADABLE)
}

/// The structure layer and the approved decisions read against it, for the two routes that need
/// both.
///
/// **One function and not two copies, because a decision's anchor set is what a stamp is anchored
/// TO.** `GET /map` computes it to say where a decision stands; `POST /map/stamps` computes it to
/// know what it is putting a verdict on. If the two ever derived it differently, a stamp would be
/// written against one anchor set and expired against another — a green that lapses the instant it
/// is given, with a diff nobody can explain, which is the false-alarm failure that costs exactly
/// the trust §7 is trying to buy.
///
/// It goes to disk, so it runs on `spawn_blocking` the way `blame` and `grep` already do: walking a
/// thousand-file tree on the async executor blocks the whole daemon for a good few milliseconds,
/// and this daemon is also answering a three-second poll. `specs_in` walks directories too and
/// rides the same hop, since the thread already has the root in hand.
///
/// A missing folder is a 404 and a failed query is a 500, which is not one distinction made twice:
/// the first is something the owner did to their own machine, the second is this daemon failing.
pub(super) async fn project_junction(
    state: &AppState,
    id: &str,
    root: PathBuf,
) -> Result<(crate::map::Structure, crate::map::join::Junction), StatusCode> {
    let walked = tokio::task::spawn_blocking(move || {
        let structure = crate::map::structure(&root)?;
        // Slugs and not paths, for the reason `get_project_map_specs` gives below: a decision row
        // carries the slug, and the slug is what `join` checks a citation's candidate against.
        // Handing it paths would make every candidate fail to name its document, which reads as
        // "§8 is unfixed" and is indistinguishable from it.
        let slugs: Vec<String> = crate::map::intent::specs_in(&root)
            .iter()
            .map(|path| crate::map::intent::spec_slug(path))
            .collect();
        Ok::<_, std::io::Error>((structure, slugs))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (structure, spec_slugs) = walked.map_err(|error| {
        // A folder that has been renamed or deleted is something its owner did, not a
        // fault of this daemon — and `ls`, `cat`, `grep` and `blame` already answer 404
        // for the very same `read_dir`. Answering 500 would put a warning in the log for
        // an ordinary Tuesday.
        if error.kind() == std::io::ErrorKind::NotFound {
            return StatusCode::NOT_FOUND;
        }
        tracing::warn!(%error, project_id = %id, "project map failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let decisions = crate::map::store::approved(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "project map decisions failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let spec_slugs = document_slugs(state, id, spec_slugs).await?;

    // The recorded anchor sets, which are the only part of this junction with a memory. Read here
    // rather than merged into the payload afterwards, because `join` needs them: a file somebody
    // wrote down as a decision's code is CLAIMED, and deciding that after the fact would mean a
    // second place that knows what §5.1's two piles mean.
    let records = crate::map::store::anchors(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the recorded anchors failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let junction = crate::map::join::join(
        &decisions,
        &structure.modules,
        &structure.foreign,
        &spec_slugs,
        &records,
    );
    Ok((structure, junction))
}

/// Which specs this project has, named the way the owner reads them.
///
/// **Slugs and not paths.** The path is where the file happens to sit; the slug is what
/// [`crate::map::intent::spec_slug`] produces, what the owner reads, what they hand back to
/// `POST /map/extract`, and what a decision row carries forever. Two projects keeping their specs
/// in different folders name one document alike.
///
/// **Empty is an answer, not an error.** A project with no specs gets a sentence about what is
/// missing, and an empty list is that sentence's input — the same posture `map` itself takes
/// about a folder with nothing in it.
///
/// [`crate::map::intent::specs_in`] probes three conventional folders rather than reading a
/// setting, and this route inherits that whole: a project matching none of them is not broken, it
/// is unconfigured in a way nobody has to configure.
///
/// On `spawn_blocking` for the reason `get_project_map` gives above: this walks directories, and
/// the daemon is also answering a three-second poll.
pub(super) async fn get_project_map_specs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;
    tokio::task::spawn_blocking(move || {
        crate::map::intent::specs_in(&root)
            .iter()
            .map(|path| crate::map::intent::spec_slug(path))
            .collect::<Vec<_>>()
    })
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// What `GET …/map/items` is asked about: one file of the project.
#[derive(Deserialize)]
pub(super) struct MapItemsQuery {
    /// Relative to the project root, with forward slashes — exactly as `GET /map` spells a module.
    ///
    /// The client sends back a path this map already gave it, which is what makes the guard below
    /// a guard against a *forged* request rather than against its own shell.
    pub(super) path: String,
}

/// What one file declares, and which of those things use each other.
///
/// **A route of its own, and never a field on `GET /map`.** The whole-project answer already walks
/// the tree, asks git for every anchor's digest and reads three tables; folding every file's items
/// into it would multiply the largest response this daemon sends by the size of the project, to
/// carry a level nobody is looking at until they click into it. This is the one place in the map
/// where laziness is not a premature optimisation but the shape of the question: a reader opens one
/// file at a time.
///
/// **The path is checked and never trusted, even though the shell only ever sends one back.** A
/// route that reads a caller-supplied path is a file-disclosure hole the moment anything but that
/// shell speaks to it, and [`crate::inspect::resolved_within`] is already the answer to exactly this
/// — including the case a lexical check cannot see, where a junction inside the project resolves
/// outside it. Reusing it rather than writing a second guard is the same rule the rest of this file
/// keeps: a check that lives in a handler is a check the second caller forgets.
///
/// **A file this reader does not understand is a `200` and not a `404`**, carrying `reader: null`
/// and no items. *I cannot read Go* and *there is no such file* are opposite answers, and the
/// structure layer already spent a paragraph refusing to collapse them — [`crate::map`]'s
/// `unread` exists for that reason. A `404` here would reopen it one level down.
pub(super) async fn get_project_map_items(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MapItemsQuery>,
) -> Result<Json<crate::map::items::Items>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;

    // On `spawn_blocking` for the reason `get_project_map` gives: this canonicalizes and reads a
    // file, and the daemon is also answering a three-second poll.
    let asked = query.path.clone();
    tokio::task::spawn_blocking(move || {
        let full =
            crate::inspect::resolved_within(&root, &query.path).map_err(|error| match error {
                crate::inspect::InspectError::UnsafePath => StatusCode::BAD_REQUEST,
                _ => StatusCode::NOT_FOUND,
            })?;
        let source = std::fs::read_to_string(&full).map_err(|_| StatusCode::NOT_FOUND)?;
        Ok(crate::map::items::items(&asked, &source))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map(Json)
}

#[derive(Deserialize)]
pub(super) struct MapExtractBody {
    /// The document's name, which is its filename without the extension — what
    /// [`crate::map::intent::spec_slug`] produces and what a decision row carries forever. NOT a
    /// path: two projects keeping their specs in different folders must name the same document the
    /// same way, and a path would also be a way to ask this route to read an arbitrary file.
    pub(super) spec_slug: String,
    pub(super) brain: String,
}

/// How many decisions one press of the triage button may put in front of a model.
///
/// **A ceiling on a wait, and it is expected to be hit rather than approached.** One model call per
/// decision: this repository's ~350 approved decisions, none of them stamped, is 350 runs of the
/// agent CLI, and the shell asks this synchronously the way `map/extract` does. Twenty is what
/// somebody watches a spinner through — minutes with a cloud brain — and it is deliberately not
/// sized to drain a backlog in one press, because nothing that drains a 350-row backlog in one
/// press finishes inside a request.
///
/// **Sized knowing the staleness rate rather than guessing it.** Counted on this repository on
/// 2026-08-26: 109 files under `core/src` and `shell/src` name a `§`, and **17 of them were touched
/// in the last 20 commits** — 26 in the last 50. An anchor set is several files, so a decision's
/// chance of going stale is HIGHER than that per-file sixth, and `map::triage::inputs_digest` covers
/// the anchor blobs precisely so it does. This repository will therefore rarely be in the "nothing
/// stale" state, and the cap will sit saturated rather than draining once.
///
/// **The silent cap is the disease, so [`TriageReport::left_over`] is not optional.** A run that
/// stopped at twenty of two hundred and said only "done" reads as *covered everything*, which is
/// §1's thousand-line plan reproduced by the feature built to cure it. The number has to reach the
/// screen, not merely the log.
///
/// **It drains rather than starves, and that is a property of the skip and not of this number.**
/// What stops the first twenty being the only twenty ever looked at is that a judgement still
/// current costs no call, so the next press starts where the last one stopped.
///
/// **Which twenty is [`crate::map::recency`]'s answer and not this constant's**, and the two were
/// separated late: the sweep used to run in `map::store::approved`'s order — `spec_slug, ordinal,
/// id` — which meant a saturated cap paid for the first twenty decisions alphabetically by
/// document, every press, for ever. §10 says the order is by recency of the anchor code's last
/// change, and until that landed this comment recorded the gap rather than pretending the order had
/// been chosen. It changes which twenty go first; it does not change whether the rest are reached.
pub(super) const MAX_TRIAGE_BATCH: usize = 20;

/// Which brain the caller named, and nothing else.
///
/// Deliberately stricter than [`crate::chats::Brain::from_wire`], which reads anything unfamiliar as
/// `Cloud`. That is right for a conversation, whose default has always been the cloud and whose
/// column carries it. It is wrong here: this is the one choice the owner explicitly asked to make,
/// and quietly making it for them — in the direction that spends money and sends the document off
/// the machine — is the wrong default to inherit.
pub(super) fn read_brain(value: &str) -> Option<crate::chats::Brain> {
    match value {
        "cloud" => Some(crate::chats::Brain::Cloud),
        "local" => Some(crate::chats::Brain::Local),
        _ => None,
    }
}

/// Ask a model what one spec decided, and leave the answer waiting for its owner.
///
/// **Synchronous, and the spec's §9.1 says so: it returns the list.** A cloud brain makes this a
/// slow request, and that is accepted — the owner pressed a button about one document and is
/// waiting for the thing they asked for. The alternative is a status row, a poll and a state the
/// page has to draw, for a wait measured in seconds.
///
/// **Nothing here is approved.** The list comes back so the owner can read it; it is already in the
/// table as a pile nobody has read, and it stays that way until they answer line by line — §4.
///
/// **A `local` request never becomes a cloud one.** `local` is what somebody chooses when the
/// document must not leave the machine, or when they are not paying for it, so a machine with no
/// local model refuses rather than falling back: falling back would break both promises at once,
/// silently, on the bill. The refusal is 503 and not 422 — the request was well formed and this
/// machine simply cannot serve it, which is a different thing to tell the owner from "you asked
/// wrong".
pub(super) async fn post_project_map_extract(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MapExtractBody>,
) -> Result<Json<Vec<crate::map::store::Decision>>, StatusCode> {
    let brain = read_brain(&body.brain).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let root = resolve_read_root(&state, &id, None).await?;

    // On `spawn_blocking` for the reason `get_project_map` gives: this walks directories and reads
    // a file that may be tens of kilobytes, and the daemon is also answering a three-second poll.
    // Finding the document and reading it are one hop because they are one answer — a slug that
    // matches nothing and a file that cannot be read are the same 404 to whoever asked, and neither
    // is a fault of this daemon.
    let wanted = body.spec_slug.clone();
    let source = tokio::task::spawn_blocking(move || {
        let path = crate::map::intent::specs_in(&root)
            .into_iter()
            .find(|path| crate::map::intent::spec_slug(path) == wanted)?;
        std::fs::read_to_string(root.join(path)).ok()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Bound out here because `Extractor::Loopback` borrows it and the borrow has to outlive the
    // `match` that creates it.
    let local_model;
    let asked = match brain {
        crate::chats::Brain::Cloud => crate::map::intent::Extractor::Cli(state.runner.as_ref()),
        // `read_brain` above (~3068) only ever yields `cloud` or `local` — this arm exists to
        // satisfy exhaustiveness over the full `Brain` type, not to describe a state this route can
        // reach today. Whoever makes the hosted route selectable HERE owns replacing it.
        crate::chats::Brain::OpenRouter => {
            crate::map::intent::Extractor::Cli(state.runner.as_ref())
        }
        crate::chats::Brain::Local => {
            // Read per request rather than cached on `AppState`, which is the argument
            // `models_config`'s own doc comment makes: a name cached at startup is one the owner
            // cannot change without restarting the daemon.
            //
            // The field is named for triage and is being reused as "the local model this machine
            // has". That reuse is the house pattern rather than a stretch — `main.rs` already
            // hands `voice_cleanup_model` to the web pillar as its quarantine model — and there is
            // exactly one Ollama model configured on a machine.
            let Some(model) = models_config().local_triage_model else {
                tracing::warn!(
                    project_id = %id,
                    spec = %body.spec_slug,
                    "the local brain was asked for and this machine has no local model configured"
                );
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            };
            local_model = model;
            crate::map::intent::Extractor::Loopback {
                client: &state.web.http,
                base_url: crate::execution::runner::OLLAMA_BASE_URL,
                model: &local_model,
            }
        }
    };

    let decisions = crate::map::intent::extract(asked, &body.spec_slug, &source)
        .await
        .map_err(|error| {
            // 502 and not 500: the daemon did its part and the thing it asked did not answer. An
            // empty list would be the other reading, and it is the one this feature exists to
            // forbid — "this spec decided nothing" is a claim, and nobody is in a position to make
            // it when nobody read the document.
            tracing::warn!(
                %error,
                project_id = %id,
                spec = %body.spec_slug,
                brain = %brain.as_str(),
                "the extraction run failed"
            );
            StatusCode::BAD_GATEWAY
        })?;

    crate::map::store::record(&state.pool, &id, &body.spec_slug, brain, &decisions)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                project_id = %id,
                spec = %body.spec_slug,
                "an extraction was read but could not be recorded"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // The whole pile and not this extraction, because the pile is what the owner reads. A window
    // handed only what just arrived would show a shrinking list every time a second spec was read.
    crate::map::store::pending(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the decision pile failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// What is waiting for this project's owner, on a route of its own.
///
/// The same list `POST …/map/extract` hands back. Separate because a window that was already open
/// has to be able to refresh the pile, and the only other way to ask for it costs a model run.
///
/// Ordered by [`crate::map::store::pending`]: oldest extraction first, because a pile read in the
/// order it arrived is a pile that ends.
///
/// The project is resolved first, the way `map` and `map/extract` do it, so an id nobody registered
/// is a 404 rather than an empty list — "nothing is waiting for you" and "there is no such project"
/// are different answers, and only one of them is true. It asks nothing of the disk beyond that:
/// `resolve_project_root` reads the row and never the folder, so a project whose folder has moved
/// still has a pile, which is exactly what somebody looking at a broken project wants.
pub(super) async fn get_project_map_decisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::map::store::Decision>>, StatusCode> {
    resolve_project_root(&state, &id).await?;
    crate::map::store::pending(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the decision pile failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// §6.2's pile: everything this project's triager has silenced, and why.
///
/// **Its own door and never a filter on the map, because §6.2 says *sempre acessível*.** A pile you
/// reach only by knowing which query parameter to add is not always accessible, and §13 rates *o
/// triador silencia o que devia mostrar* a **real** residual risk whose only mitigation is that
/// this stays visible. A mitigation with a secret entrance is not one.
///
/// **Filtered by NEITHER standing, staleness nor retirement — the exact opposite of what `GET /map`
/// does with the same table, and all three departures are deliberate.** That route answers *what is
/// true now*: a judgement about a decision the owner has since stamped, or about code that has
/// since moved, is not an answer to its question, and a retired decision is not in its map at all
/// — see [`crate::map::triage::reconcile`], which is where §5.3's numbers would otherwise stop
/// reconciling. This route answers *what the triager did*, and each of the three filters would
/// delete part of that record:
///
/// - **stamped** — the stamp is the evidence the silence was premature, so the silencing is worth
///   MORE afterwards, not less;
/// - **stale** — a silence whose reason was written about code that has since moved is exactly the
///   one somebody should reread;
/// - **retired** — auditing the triager does not care whether the decision survived, and a
///   silencing that vanished because the owner later said no is a bug report deleted by its own
///   subject. It is reported instead, as `retired` on the row.
///
/// Somebody will one day read this as an inconsistency and repair it: the repair breaks §6.2 in one
/// direction or §5.3 in the other, and
/// `the_silenced_pile_still_shows_a_silencing_of_a_decision_that_was_later_stamped` and
/// `a_silencing_survives_the_decision_being_retired` are the tests that stop it.
///
/// **Every silencing ever written and not the latest judgement per decision**, which is
/// [`crate::map::store::silencings`] rather than `judgements` and is what §6.2 literally asks for:
/// *"a razão de **cada** silenciamento"*. A decision silenced last week and flagged today has a
/// silencing in the table, and until this route read the history it had none in the pile —
/// `map_triage` was made append-only for precisely that row and nothing was reading it.
///
/// Asks nothing of the disk: `resolve_project_root` reads the row and never the folder, so a
/// project whose folder has moved still has its pile — which is *sempre acessível* meaning what it
/// says. An id nobody registered is a 404 rather than an empty list, for the reason
/// [`get_project_map_decisions`] gives: *nothing was silenced* and *there is no such project* are
/// different answers and only one of them is true.
///
/// The order is the store's — newest first, ties broken by insertion order — and the argument for
/// it is [`crate::map::store::silencings`]'s. Sorted in SQL and not here, because with the whole
/// history in the pile the tie-break has to be `id` and a handler cannot see one.
///
/// **Capped at [`crate::map::store::SILENCED_PAGE`], with the uncapped total on the answer.** *Sempre
/// acessível* is not *all at once*, and this table only grows: append-only by design, by up to
/// `MAX_TRIAGE_BATCH` rows per press of the button the feature exists to encourage, on a route the
/// window performs every time the map opens. The write side was bounded from the first commit and
/// this side was bounded by nothing. What makes the cap admissible rather than a quiet truncation is
/// that the reader is handed the number it was cut from — the same bargain
/// [`TriageReport::left_over`] strikes one route over, and the same one every capped pile in the
/// window keeps.
///
/// **It answers [`crate::map::store::Silencing`] rows and nothing wrapped around them, and the
/// wrapper's removal is a correction rather than a tidy-up.** A `SilencedRow` here added
/// `machine_written`, which could not be `true` on this route: the one producer of a daemon-written
/// reason is [`crate::map::triage::unreadable_flag`], which may only ever reach
/// [`crate::map::triage::Judgement::Flagged`], while [`crate::map::store::silencings`] selects the
/// silenced. So the payload where the answer is provably always `false` carried the field and
/// `GET /map`'s `triage` — the only place a `nucleos:` sentence can appear at all — carried nothing,
/// which made a client re-spell [`crate::map::triage::DAEMON_MARK`] in its own language to keep §6.2's
/// attribution. It now lives on [`crate::map::store::Judged`], computed at the only place a row of
/// that table becomes a value. A field that can never be true is worse than an absent one: it is a
/// promise somebody eventually relies on, and this one would have been relied on for the wrong pile.
pub(super) async fn get_project_map_silenced(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::map::store::SilencedPile>, StatusCode> {
    resolve_project_root(&state, &id).await?;

    crate::map::store::silencings(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the silenced pile failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// What `GET …/map/orphan` is asked about: one section of one document.
#[derive(Deserialize)]
pub(super) struct MapOrphanQuery {
    /// The document, as `map/specs` spells it and as a decision row carries it.
    pub(super) slug: String,
    /// The heading the decision was copied from, **verbatim** — `## 4.1 Três tipos de decisão`.
    ///
    /// The heading and not the number, so that [`crate::map::join::section_number`] stays the one
    /// place that knows how one becomes the other. A client that sent the number would be a second
    /// implementation of that rule, and the day the two disagreed the guard would be searching the
    /// history for a section the junction never anchored anything to.
    pub(super) section: String,
}

/// Whether a decision with no code was never built, or lost the comment that anchored it (§14).
///
/// **Three refusals, and each is a different sentence.** `422` for a heading carrying no number:
/// that is [`crate::map::join::Anchor::Unnumbered`], where no search ever ran, and answering
/// *nothing ever named it* about a question nobody could ask would be the silent wrong answer this
/// whole feature refuses. `404` for a slug this project has no document for — the same answer
/// `map/extract` gives it, and for its reason. Everything else is a `200` carrying one of
/// [`crate::map::orphan::Orphan`]'s six states, including the two that mean *I could not look*: a
/// git that would not answer is a finding and not a fault of this daemon.
///
/// **There is deliberately NO refusal for a document that is not on disk**, and the first version
/// of this route had one. It answered `404` when the slug was missing from `map::intent::specs_in`,
/// which made the guard the only part of this map that stops working when a spec file is archived
/// or renamed — `GET /map` keeps showing those decisions, because their text was copied into the
/// table at extraction and the folder was never where they lived. Worse, the slug IS the filename,
/// so renaming a spec would have killed the guard for every decision of that document while the
/// rows carrying the old slug sat on screen looking answerable. See [`document_slugs`]: the list of
/// this project's documents comes from the table first, so a slug an approved decision carries is a
/// document this project has, whatever happened to the file.
pub(super) async fn get_project_map_orphan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MapOrphanQuery>,
) -> Result<Json<crate::map::orphan::Orphan>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;

    let Some(section) = crate::map::join::section_number(&query.section) else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };

    // On `spawn_blocking` for `map/extract`'s reason: this reads a directory, and the daemon is
    // also answering a three-second poll. Slugs and not paths, exactly as `project_junction` does
    // it, because the slug is what a citation's candidate is checked against.
    let listing = root.clone();
    let on_disk: Vec<String> = tokio::task::spawn_blocking(move || {
        crate::map::intent::specs_in(&listing)
            .iter()
            .map(|path| crate::map::intent::spec_slug(path))
            .collect()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let slugs = document_slugs(&state, &id, on_disk).await?;

    Ok(Json(
        crate::map::orphan::orphan(&root, &section, &query.slug, &slugs).await,
    ))
}

#[derive(Deserialize)]
pub(super) struct MapDecisionBody {
    /// `true` approves the line and lets it into the map; `false` retires it, which is what stops
    /// it being proposed for that spec again. Required, with no `serde(default)`: a body that omits
    /// it is a client that has not decided, and defaulting would decide for them in a direction.
    pub(super) approved: bool,
}

/// The owner's answer to ONE line.
///
/// **No bulk form, and that is §4 rather than an omission.** The list is approved line by line; a
/// route that took the whole list would be the thousand-line plan again, wearing a smaller shape.
///
/// `204` when a row changed and `404` when none did. [`crate::map::store::decide`] returns `false`
/// for three different reasons — a line belonging to another project, an id that never existed, and
/// a line somebody already answered — and they are deliberately one answer here. All three mean
/// *that line is not yours to answer now*; telling them apart would tell a caller which ids exist
/// in projects it cannot see; and none of them is a fault of this daemon worth a 500.
///
/// **No `resolve_project_root` here, unlike the GET above.** `decide` puts `project_id` in its own
/// `WHERE` and its doc comment argues for exactly that — a check that lives in a handler is a check
/// the second caller forgets. An unregistered project therefore already gets this route's 404, from
/// the store, and a resolve on top would be a second query that cannot change the answer.
/// `delete_project_command`, the same shape one pillar over, relies on its store for the same
/// reason.
pub(super) async fn post_project_map_decision(
    State(state): State<AppState>,
    Path((id, decision)): Path<(String, i64)>,
    Json(body): Json<MapDecisionBody>,
) -> Result<StatusCode, StatusCode> {
    match crate::map::store::decide(&state.pool, &id, decision, body.approved).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, project_id = %id, decision, "answering a decision failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Deserialize)]
pub(super) struct MapStampBody {
    pub(super) decision_id: i64,
    /// The owner's verdict, parsed by serde and by nothing this handler wrote.
    ///
    /// Typed rather than taken as a `String` and read afterwards, which is where `read_brain` next
    /// door had to end up and why the two differ. `Brain::from_wire` reads anything unfamiliar as
    /// `Cloud`, so that route needed a stricter reader of its own; [`crate::map::stamp::Verdict`]'s
    /// derived `Deserialize` is already exact — §5.2 has three and `0124`'s CHECK admits three — so
    /// a fourth is refused before this function runs at all. That is the earliest place it can be
    /// refused, and the only one where nothing has yet had a chance to guess what was meant.
    pub(super) verdict: crate::map::stamp::Verdict,
    /// Obligatory on amber and optional on the other two (§7).
    ///
    /// `serde(default)` so a client that has nothing to say may omit the field rather than having
    /// to spell `null`. The default is `None` and never an empty string, because *said nothing* and
    /// *said the empty string* are different, and `0124` stores the first as NULL.
    #[serde(default)]
    pub(super) note: Option<String>,
}

/// The owner's verdict on one decision, and the moment they gave it (§5.2, §7).
///
/// **The daemon computes the digest and the body has no field for one.** A client that could supply
/// it is a client that could mint a green anchored to a digest nothing will ever move — a stamp that
/// never expires, which is §1's false confidence with a signature on it. The anchor set comes from
/// [`project_junction`], the same reading `GET /map` expires the stamp against, so the two cannot
/// disagree about what this verdict was given over.
///
/// Four answers, and each is a different thing to tell whoever asked:
///
/// - `204` when a row was written. Nothing to say back; §9.2 makes the current state the last row,
///   and the map is where it is read.
/// - `404` when nothing was written — a decision belonging to another project, an id that names
///   nothing, or a line nobody approved. All three mean *that decision is not yours to stamp now*,
///   none is a fault of this daemon, and telling them apart would tell a caller which ids exist in
///   projects it cannot see. §4 is why the third is in that list at all: approval is the gate, and a
///   verdict on an unapproved line would put the owner's word on something they never let in.
/// - `400` for an amber with nothing in its note. `0124`'s CHECK is the backstop and would surface
///   as a 500, which reads as this daemon having broken; a malformed request is the client's fault
///   and has to read as one. §5.2 makes the note the whole of amber — *falta migrar as páginas de
///   pilar* is worth more than the colour is — so an empty one is not a lesser amber, it is not one.
/// - `503` for a `settled` when git is there and **would not answer**. **Not `500`**: that is
///   transient, and the honest thing to tell the owner is *try again*, whereas a 500 reads as a
///   fault here and puts a warning in a log for an ordinary Tuesday. §7.1 makes *está como quero*
///   the only verdict the code moving can falsify, so it is the only one that may not be recorded
///   without knowing what it is anchored to; the other two take a NULL digest without complaint,
///   because neither has an expiry the code can reach. **A project with no git repository is not
///   this case** and gets a `204` — see the `NoRepository` arm below, and [`crate::map::stamp::Anchors`]
///   for what telling the two apart cost when they were one value.
///
/// **The tree walk on this path is deliberate, and it is the second one this click pays for.** The
/// shell will do `GET /map`, then this, then invalidate and `GET /map` again — three walks of a
/// thousand-file tree per stamp. It is not an oversight and it must not be optimised away by
/// computing the anchor set from anything cheaper: the write has to derive it **exactly** as the
/// read expires it, or a stamp is anchored to one set and compared against another and lapses the
/// instant it is given, with a diff nobody can explain. That is the false-alarm failure that costs
/// precisely the trust §7 is buying. In particular, an optimistic update in the shell may show the
/// verdict immediately and may **not** invent the `Standing` that goes with it — `Watch` is decided
/// by a git call and by the join, and a client guessing `Watched` would put a certain word on screen
/// over an anchor set nobody has read yet.
///
/// **The decision is looked up here, and that is not the handler check [`crate::map::store::decide`]
/// argues against.** `map::store::stamp` still puts `project_id`, `approved_at` and `retired_at` in
/// its own `WHERE`, and that INSERT is what actually refuses the write — this lookup cannot replace
/// it and does not try. It is forced: without finding the decision there is no anchor set to compute
/// a digest from. Short-circuiting on it also fixes an ordering that would otherwise be wrong, and
/// the wrongness is quiet: a settled verdict on a decision that does not exist, in a folder that is
/// not a repository, would answer `503` — *git is unreadable, try again* — about a request that will
/// never succeed however healthy git becomes.
pub(super) async fn post_project_map_stamp(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MapStampBody>,
) -> Result<StatusCode, StatusCode> {
    // Trimmed with `str::trim` against a CHECK that strips space, tab, CR and LF. Stricter, and
    // deliberately in that direction: a note of one non-breaking space is one the table would take
    // and one that says nothing, and the failure that matters is being LOOSER than the CHECK, which
    // turns a client's mistake into a 500 about this daemon.
    let note = body.note.as_deref();
    if body.verdict == crate::map::stamp::Verdict::Partial
        && !note.is_some_and(|note| !note.trim().is_empty())
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let root = resolve_read_root(&state, &id, None).await?;
    let (_, junction) = project_junction(&state, &id, root.clone()).await?;
    let anchored = junction
        .decisions
        .iter()
        .find(|anchored| anchored.decision_id == body.decision_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    // `watched` and not `modules`: once a set has been written down, that is what the green expires
    // against. See `anchor_digests`, which makes the same substitution for the same reason.
    let anchors = anchored.watched();
    // **A stamp never overwrites a set the OWNER pointed at**, and that is the one rule this
    // side effect has. What a stamp can record is whatever the `§` comments happened to say at this
    // instant — which, while §8 is unfixed, is a set of guesses about which document a bare `§`
    // meant. Writing that over a choice somebody made deliberately would demote the choice to the
    // guess, silently, at the moment they were being most careful. So an owner record stands, and
    // the stamp adds nothing.
    let already_chosen = matches!(
        anchored.record.as_ref().map(|record| record.source),
        Some(crate::map::store::AnchorSource::Owner)
    );

    // `Anchored::modules` and never `foreign` beside it. `Anchor::Declared` spends a paragraph on
    // why: watching an anchor means being able to read it, and promising an expiry over a Go file
    // this map cannot parse is a promise the read side cannot keep.
    let digest = match crate::map::stamp::digest(&root, &anchors).await {
        crate::map::stamp::Anchors::Computed(text) => Some(text),
        // §11: a project added from outside may have no git repository at all, and that is not a
        // fault of anything. The stamp is **allowed**, and `''` is not a lie here — with no
        // repository there is genuinely nothing that could ever move, which is the same claim `''`
        // makes for a decision no module names. Refusing instead would make this feature useless on
        // exactly the projects §11 says must still get an honest answer, and refusing it with a
        // `503` would tell their owner to try again at something that cannot succeed.
        // `Watch::NoRepository` is what carries the reason back out, so the green is visible as one
        // that will never come and ask rather than passing for one that will.
        crate::map::stamp::Anchors::NoRepository => Some(String::new()),
        crate::map::stamp::Anchors::Failed => None,
    };
    if digest.is_none() && body.verdict == crate::map::stamp::Verdict::Settled {
        tracing::warn!(
            project_id = %id,
            decision = body.decision_id,
            anchors = anchors.len(),
            "a green was asked for and git would not say what its anchors are, so it was refused"
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    match crate::map::store::stamp(
        &state.pool,
        &id,
        body.decision_id,
        body.verdict,
        digest.as_deref(),
        note,
    )
    .await
    {
        Ok(true) => {
            // After the stamp and never instead of it: the verdict is what the caller asked for,
            // and a record written for a stamp that then failed to land would be a memory of an
            // act that did not happen. A record that fails to write is logged and swallowed for the
            // mirror-image reason — the owner's verdict IS recorded, and answering `500` would tell
            // them their stamp did not land when it did.
            if !already_chosen
                && let Err(error) = crate::map::store::record_anchor(
                    &state.pool,
                    &id,
                    body.decision_id,
                    &anchors,
                    crate::map::store::AnchorSource::Stamp,
                )
                .await
            {
                tracing::warn!(
                    %error,
                    project_id = %id,
                    decision = body.decision_id,
                    "the stamp landed and its anchor set could not be written down"
                );
            }
            Ok(StatusCode::NO_CONTENT)
        }
        // Reachable despite the lookup above: the line could have been retired between the two, and
        // the store is the authority on that rather than the reading this handler took a moment ago.
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(
                %error,
                project_id = %id,
                decision = body.decision_id,
                verdict = body.verdict.as_str(),
                "stamping a decision failed"
            );
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Deserialize)]
pub(super) struct MapTriageBody {
    /// Which brain answers, exactly as `POST /map/extract` takes it. No spec slug and no decision
    /// id beside it: §10 gives the triager one scope — everything nobody has stamped — and a route
    /// that let a caller name the decisions would let it name the ones whose answer it liked.
    pub(super) brain: String,
}

/// What one triage run did, and — the half that is easy to leave out — what it did not.
///
/// **Counts and never a bare `204`.** *"Corri o triador e não aconteceu nada"* and *"corri o triador
/// e estava tudo em dia"* are different facts about a project, and a route that reported only
/// success would make the second look like the first. Every field below is a thing that would
/// otherwise have happened invisibly.
///
/// **The seven after [`Self::in_scope`] are exhaustive and disjoint, and a test asserts they add
/// up.** §5.3 makes that the house rule for anything a person reads as a total — *as categorias são
/// exaustivas e disjuntas, e um teste verifica que somam ao total em vez de um comentário o
/// prometer* — and this is a report, which is exactly where a reader stops checking.
///
/// **Every one of them is a count and none is a flag**, which is the correction this type needed
/// most. [`Self::unreadable_anchors`] was a `git_would_not_answer: bool` for one commit, and a
/// boolean can say that a run was narrower than it looks without ever saying by how much — which
/// leaves the owner reading `judged: 3` off a project of three hundred with no way to learn that
/// the other two hundred and ninety-seven were skipped.
#[derive(Debug, Default, Serialize)]
pub(super) struct TriageReport {
    /// Decisions in [`crate::map::stamp::Standing::Never`], which is the whole of what the triager
    /// is allowed to look at (§10). Everything below is a bucket of this number.
    pub(super) in_scope: usize,
    /// Judged already, against inputs that have not moved since. Cost nothing and asked nothing.
    pub(super) already_current: usize,
    /// Answered and written this run. The only field that grew the table.
    pub(super) judged: usize,
    /// The model answered, the answer was not a judgement, and **a `flagged` row was written
    /// saying so**. Rows written this way, not rows withheld — the field changed meaning when the
    /// starvation was fixed, and the sentence is spelled out because the old one read the other way
    /// round.
    ///
    /// Counted apart from [`Self::unanswered`] because the two send whoever is debugging to
    /// different places: this one to the prompt, that one to the machine. See
    /// [`crate::map::triage::unreadable_flag`] for why the row exists at all and what it costs.
    pub(super) unreadable: usize,
    /// Nobody answered: the run itself failed. A brain that is down fails on every decision and a
    /// brain that is confused fails on one, so folding this into [`Self::unreadable`] would report
    /// an unreachable machine as a broken prompt and send somebody to rewrite a question that was
    /// never asked.
    pub(super) unanswered: usize,
    /// The decision stopped being one this project's map holds between the reading and the write.
    ///
    /// Expected to be zero, and counted rather than dropped because the six have to add up:
    /// approval cannot be taken back today — `decide` only touches rows nobody has answered and
    /// `record` supersedes only what is still pending — so nothing can retire an approved decision
    /// mid-run. A number here means that changed, which is worth finding out from a report rather
    /// than from a header that stopped reconciling.
    pub(super) vanished: usize,
    /// Stale, and the cap stopped the run before reaching them. **The field this report exists
    /// for.** See [`MAX_TRIAGE_BATCH`].
    pub(super) left_over: usize,
    /// In scope, and **skipped because git would not say what their anchor code is**.
    ///
    /// **Skipped and not triaged, which dissolves a storm rather than reporting one.** An earlier
    /// version of this route triaged them anyway and put a boolean on the report. The trouble is
    /// not the answer — the model reads none of its evidence from git, so an answer given during an
    /// outage is as good as any other — it is the digest: `map::triage::inputs_digest` keeps all four
    /// states of [`crate::map::stamp::Anchors`] apart on purpose, which is what `0124` was amended to
    /// require, so a judgement recorded under `git-failed` dies the moment git recovers. Every row
    /// written during an outage would have to be bought again.
    ///
    /// Skipping removes the source instead of the symptom. No judgement is ever written carrying a
    /// `git-failed` digest, so there is nothing to invalidate on recovery; the judgements already
    /// stored read stale for the length of the outage and are skipped rather than re-run, so nothing
    /// is spent; and when git answers again they match their `Computed(...)` digest and are current,
    /// untouched.
    ///
    /// It is the posture slice 4 already took with `Lapse::Unreadable`: *I could not look* is
    /// neither *unchanged* nor *changed*, and acting on either guess is the error.
    ///
    /// **Not the same as §11's project with no repository**, which is
    /// [`crate::map::stamp::Anchors::NoRepository`] and is triaged normally — with no repository
    /// there is genuinely nothing that could ever move, which is a permanent fact and an honest
    /// digest. Nor does it touch a decision no module names: that is `Computed("")`, reached without
    /// git being asked at all.
    pub(super) unreadable_anchors: usize,
}

/// Ask a model which of the decisions nobody has stamped deserve their owner's eyes (§6, §10).
///
/// **Only [`crate::map::stamp::Standing::Never`], and that is §10's table rather than a shortcut.**
/// A decision whose anchor code moved — *"se estava carimbada, caducou (§7.1); se nunca foi vista,
/// o triador decide se vale o teu olhar."* A stamped decision already carries the owner's answer and
/// a lapsed one already reaches them through §5.3's `J`, so running a model over either is the model
/// second-guessing a human: the one authority §6 takes away from it, recovered through a sweep.
///
/// **And only rows whose judgement is stale or absent.** `map::triage::is_current` against a digest
/// computed now, so a second press over a repository nothing has happened to writes nothing and
/// spends nothing. That is not an optimisation: without it the bill grows with how often somebody
/// presses a button rather than with what changed, and a feature whose cost is unrelated to its
/// value is one people stop pressing.
///
/// **The anchor material is [`project_junction`] and [`anchor_digests`], the same two `GET /map`
/// reads.** Deriving it a second way here would be two answers to one question, and the disagreement
/// would arrive as a triage answer about files the map does not consider anchors — visibly
/// approximate is this feature's bargain, and two mechanisms quietly disagreeing is not that.
///
/// **A failure on one decision never abandons the rest.** A run that gave up at the first
/// unparseable answer would be useless on precisely the repositories worth running it on: three
/// hundred decisions is three hundred chances for a model to answer something odd. Each failure is
/// counted, logged with what was actually said, and the sweep carries on. That is also why the
/// route does not answer `502` the way `map/extract` does when the model fails — by then rows have
/// been written, and a status code that discarded the report would hide work actually done.
///
/// **And a decision git would not answer for is skipped rather than triaged**, which is the one
/// rule here that is about money rather than about correctness. See
/// [`TriageReport::unreadable_anchors`].
///
/// **The way out of a flag is a stamp, and §5.2 already has the right one — so no panel built on
/// this route may grow a dismiss button.** A flag stays in §5.3's `J` until its inputs move, which
/// looks like a nag with no answer until you notice that the answer is *a meio, e eu sei*, whose
/// stated purpose is *"converte um não sabia num sabia, que é metade da cura"*. The owner who reads
/// a flag and decides it is noise says so in that note. Dismissing without saying anything is
/// precisely the *não sabia* this map exists to convert, and a fourth verdict for it would be §5.2
/// widened to make the map quieter — which is the one direction it must never be widened in.
///
/// `200` with the report, and the four refusals are the ones `map/extract` already makes: `422` for
/// a brain nobody can read, `404` for a project or a folder that is not there, `503` for a `local`
/// this machine has no model for — never a quiet fall back to the cloud, which would break the two
/// promises that word carries at once — and `500` for the daemon's own failure to read or write its
/// tables.
/// Which files one decision's code is, as the owner says it.
#[derive(Deserialize)]
pub(super) struct MapAnchorBody {
    pub(super) decision_id: i64,
    /// Repository-relative, forward slashes. **An empty list is legal and is an assertion**,
    /// not a no-op: *these files were this decision's and now none are* is what somebody says
    /// about code they genuinely removed, and it is a different fact from never having written
    /// anything down.
    pub(super) paths: Vec<String>,
}

/// Write down which files one decision's code is (§14).
///
/// **The one act in this map that gives an anchor a memory.** Everything else here is
/// recomputed from the working tree on every read: delete the `§7.1` from a file and the
/// association is gone, and the decision lands in §5.1's *declarado, sem código* looking exactly
/// like one nobody ever implemented. This is what makes that a named alarm instead of a silent
/// disappearance.
///
/// **Every path is resolved inside the project before anything is written**, through the same
/// [`crate::inspect::resolved_within`] `cat` and `grep` use — so an absolute path, a `..`, and a
/// junction pointing out of the tree are all refused by the function that already knows how,
/// rather than by a second rule written here. A path that does not exist is refused too, and
/// that is not tidiness: an anchor naming a file the project does not have would sit in
/// [`crate::map::stamp::Lapse::Moved`]'s `gone` for ever, nagging about a file that was never
/// there.
///
/// **Deliberately not restricted to files the map can READ.** [`crate::map::scanned`]
/// decides whose `§` this reader parses; it has no standing over which files somebody’s code
/// lives in. A Go sidecar, a migration, a stylesheet — the owner is entitled to say a
/// decision's code is there, and `map::stamp::digest` watches any tracked path perfectly well.
/// Refusing them would make this route unusable for exactly the languages §11 already admits
/// the map cannot read.
///
/// `204` when a row was written, `404` when the decision is not this project’s, not approved,
/// or retired — [`crate::map::store::decide`]'s three-reasons-one-answer, for its reason.
pub(super) async fn post_project_map_anchors(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MapAnchorBody>,
) -> Result<StatusCode, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;

    let asked = body.paths.clone();
    let checked = tokio::task::spawn_blocking(move || {
        let mut kept: Vec<String> = Vec::with_capacity(asked.len());
        for path in &asked {
            // Backslashes normalised before anything looks at the string, because the map
            // spells every path with forward slashes (`map::collect` does the same
            // `replace`) and a record written the other way would never match an anchor again.
            let cleaned = path.trim().replace('\\', "/");
            if cleaned.is_empty() {
                return None;
            }
            let resolved = crate::inspect::resolved_within(&root, &cleaned).ok()?;
            // A directory is not a decision’s code, and recording one would put a path in the
            // digest that `git ls-files` never answers for — a permanent `gone`.
            if !resolved.is_file() {
                return None;
            }
            kept.push(cleaned);
        }
        Some(kept)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;

    match crate::map::store::record_anchor(
        &state.pool,
        &id,
        body.decision_id,
        &checked,
        crate::map::store::AnchorSource::Owner,
    )
    .await
    {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(
                %error,
                project_id = %id,
                decision = body.decision_id,
                files = checked.len(),
                "writing down a decision's anchor files failed"
            );
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub(super) async fn post_project_map_triage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MapTriageBody>,
) -> Result<Json<TriageReport>, StatusCode> {
    let brain = read_brain(&body.brain).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let root = resolve_read_root(&state, &id, None).await?;
    let (_, mut junction) = project_junction(&state, &id, root.clone()).await?;
    let (readings, _) = anchor_digests(&root, &junction.decisions).await;

    // **§10's order, and on this route it is a spending decision rather than a display one.** The
    // sweep below caps at [`MAX_TRIAGE_BATCH`] and spends one model call per decision, so the order
    // it walks in decides which twenty of a three-hundred-row backlog get money spent on them —
    // every press, for as long as the cap stays saturated, which `MAX_TRIAGE_BATCH`'s own
    // measurement says is the state this repository lives in. `map::store::approved`'s order is
    // `spec_slug, ordinal, id`, so what that bought was the first twenty decisions alphabetically by
    // document. The answer is discarded here and kept by `GET /map`: what this route needs is the
    // order, and what the panel needs is the order plus the window it is a fact about.
    let walked = crate::map::recency::walk(&root).await;
    let _ = crate::map::recency::order(&mut junction.decisions, &walked);

    let stamped = crate::map::store::stamps(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "project map stamps failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let latest: std::collections::BTreeMap<i64, &crate::map::store::Stamp> = stamped
        .iter()
        .map(|stamp| (stamp.decision_id, stamp))
        .collect();
    // Read once, before any call. A judgement written by this very run cannot make one of its own
    // decisions "already current" — every decision is visited once — and reading per decision would
    // be one query per model call for an answer that cannot change under us.
    let answered = crate::map::store::judgements(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading the triage pile failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let known: std::collections::BTreeMap<i64, &crate::map::store::Judged> = answered
        .iter()
        .map(|judged| (judged.decision_id, judged))
        .collect();

    // Bound out here because `Extractor::Loopback` borrows it and the borrow has to outlive the
    // `match` that creates it, exactly as `map/extract` has it.
    let local_model;
    let (asked, model) = match brain {
        crate::chats::Brain::Cloud => (
            crate::map::intent::Extractor::Cli(state.runner.as_ref()),
            // The brain and not a model name, and the limit is worth stating rather than hiding
            // behind a plausible string. §6.2 asks for *o modelo que o produziu*; the CLI chooses
            // its own model per run and `CommandRunner::model_for_stage` answers `None` for an
            // unnamed stage, so the honest thing this daemon knows is which brain was asked.
            // `map_decisions.brain` records exactly the same word about the other half of the
            // feature, so the two piles are attributable alike.
            brain.as_str().to_owned(),
        ),
        // Unreachable today, and refused rather than given a placeholder. `read_brain` (~4050)
        // yields only `cloud` or `local`, so a hosted brain never gets this far — the 422 below is
        // the same answer it already receives one layer up, written here rather than invented.
        //
        // **Deliberately not the shape its sibling at ~4106 uses.** That route's arm falls back to
        // the CLI extractor, which is harmless there because the answer is returned and forgotten.
        // This one WRITES what answered into the triage pile, and a row recorded against the wrong
        // brain is exactly the silent lie `Brain::OpenRouter` was split out of `Local` to prevent.
        // A pile is read months later by somebody asking where their words went.
        //
        // Whoever makes the hosted route selectable here owns replacing this with a real extractor,
        // and will find no extractor to reach for: `map::intent::Extractor` has `Cli` and `Loopback`
        // and nothing hosted. That is the work, not this arm.
        crate::chats::Brain::OpenRouter => {
            tracing::warn!(
                project_id = %id,
                "the hosted brain was asked to triage, which this route has no extractor for"
            );
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }
        crate::chats::Brain::Local => {
            // Read per request rather than cached on `AppState`, for `map/extract`'s reason: a name
            // cached at startup is one the owner cannot change without restarting the daemon.
            let Some(model) = models_config().local_triage_model else {
                tracing::warn!(
                    project_id = %id,
                    "the local brain was asked to triage and this machine has no local model \
                     configured"
                );
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            };
            local_model = model;
            (
                crate::map::intent::Extractor::Loopback {
                    client: &state.web.http,
                    base_url: crate::execution::runner::OLLAMA_BASE_URL,
                    model: &local_model,
                },
                // Here the model IS known, which is why the column takes a string rather than a
                // brain: the pile is more useful naming what answered than naming the choice.
                local_model.clone(),
            )
        }
    };

    // One clock for the whole run, as `GET /map` uses one for the whole answer: two amber stamps on
    // either side of `NOTE_LIFETIME` must be judged against the same instant, or a decision could
    // leave `Never` halfway through a sweep and be both in scope and not.
    let now = chrono::Utc::now();
    let mut report = TriageReport::default();
    let mut attempted = 0usize;

    // In §10's order, sorted above: *"dentro do que chega, a ordem é por recência de alteração do
    // código âncora, não por importância."* What that changes here is which decisions the cap
    // reaches, not whether the rest are ever reached — a judgement still current costs no call, so
    // the next press starts where this one stopped, which is the drain `MAX_TRIAGE_BATCH` describes.
    for anchored in &junction.decisions {
        let current = anchor_reading(&readings, anchored.decision_id);
        let standing = crate::map::stamp::standing(
            latest.get(&anchored.decision_id).copied(),
            crate::map::stamp::Anchoring {
                current,
                named: anchored.modules.len(),
                declared: anchored.anchor == crate::map::join::Anchor::Declared,
            },
            now,
        );
        if !matches!(standing, crate::map::stamp::Standing::Never) {
            continue;
        }
        report.in_scope += 1;

        // Before the staleness test and not after it, and the order is the point. During an outage
        // `is_current` says stale about every judgement in the project — they were stored against
        // `Computed(...)` and the reading now is `git-failed` — so a check that came second would
        // report three hundred decisions as freshly in need of a model when the only thing that
        // happened is that git hiccuped. Counted here, the number says what it is: the run was this
        // much narrower than it looks.
        if matches!(current, crate::map::stamp::Anchors::Failed) {
            report.unreadable_anchors += 1;
            continue;
        }

        let evidence = crate::map::triage::Evidence {
            decision: anchored,
            anchors: current,
        };
        let digest = crate::map::triage::inputs_digest(evidence);
        if known
            .get(&anchored.decision_id)
            .is_some_and(|judged| crate::map::triage::is_current(&judged.inputs_digest, &digest))
        {
            report.already_current += 1;
            continue;
        }

        // The cap counts ATTEMPTS and not rows, so a batch of answers nobody could read still ends
        // the run — otherwise a model returning prose would be asked three hundred times, and the
        // ceiling would protect the table instead of the owner's afternoon.
        if attempted >= MAX_TRIAGE_BATCH {
            report.left_over += 1;
            continue;
        }
        attempted += 1;

        let said = match crate::map::triage::ask(asked, evidence).await {
            Ok(said) => said,
            Err(error) => {
                tracing::warn!(
                    %error,
                    project_id = %id,
                    decision = anchored.decision_id,
                    brain = %brain.as_str(),
                    "a triage run failed, so nobody looked at this decision"
                );
                report.unanswered += 1;
                continue;
            }
        };
        // **An answer nobody could read becomes a `flagged` row that says so, and does not become
        // nothing.** Writing no row leaves the decision *not looked at*, which is honest and which
        // starves the queue: with no record that it was attempted it consumes a slot of
        // [`MAX_TRIAGE_BATCH`] on every future run, so twenty decisions that reliably confuse the
        // model mean nothing behind them is ever triaged again — a map that has stopped working
        // while going on reporting honestly that it has. See [`crate::map::triage::unreadable_flag`],
        // which argues why `flagged` is the one verdict a machine may reach for and what the trade
        // costs.
        let (answer, readable) = match crate::map::triage::parse_answer(&said) {
            Ok(answer) => (answer, true),
            Err(why) => {
                // What the model actually said travels into the log AND into the row, because
                // §6.2's argument applies hardest to the answers that were not judgements: a bug is
                // only fixable if it is visible.
                tracing::warn!(
                    project_id = %id,
                    decision = anchored.decision_id,
                    brain = %brain.as_str(),
                    "the triager answered something that is not a judgement: {why}"
                );
                (crate::map::triage::unreadable_flag(&why, &said), false)
            }
        };

        let wrote = crate::map::store::triage(
            &state.pool,
            &id,
            anchored.decision_id,
            answer.judgement,
            &answer.reason,
            &model,
            &digest,
        )
        .await
        .map_err(|error| {
            // The table refusing the row itself — a blank reason, a blank model, a blank digest —
            // is a bug in this handler and not a fact about the decision, so it is not counted and
            // swept past. `parse_answer` already refuses everything `0125` would, so reaching here
            // means the two disagree, and that is worth stopping for.
            tracing::warn!(
                %error,
                project_id = %id,
                decision = anchored.decision_id,
                "recording a judgement failed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        if !wrote {
            report.vanished += 1;
        } else if readable {
            report.judged += 1;
        } else {
            report.unreadable += 1;
        }
    }

    Ok(Json(report))
}
