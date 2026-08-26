//! What the triager is allowed to answer.
//!
//! The model enters this feature twice and is a compressor both times (§6). Before, it turns a
//! thousand lines of spec into a dozen lines of decision, and `map_intent.rs` is that half. After —
//! here — it looks at one node with the mechanical proof beside it and answers a single question:
//! *isto merece o olhar dele?*
//!
//! Split from `map_store.rs` for the reason `map_intent.rs` is: the prompt and the parse are the
//! part worth exercising without a database, and the SQL is the part worth exercising without a
//! model. The prompt, the parse and the staleness digest land here next; today the module holds
//! [`Judgement`] alone, which is what `map_store::triage` and `0119`'s CHECK are both written
//! against — the same order slice 4 took, where [`crate::map_stamp::Verdict`] arrived a task before
//! the module around it.
//!
//! **The one rule this module exists to make unsayable: the triager may never approve.** §6's table
//! names exactly one forbidden column for it — *Aprovar* — and §6.1 says why the prohibition has to
//! survive rendering as well as storage: *"se colapsassem, a autoridade que foi retirada ao modelo
//! era-lhe devolvida pela porta da renderização — e o mapa passava a ser a falsa confiança de novo,
//! agora com autoridade de semáforo."* A silenced node is *sem sinal de problema*, which is a claim
//! about **the triager** and about nothing else.

use serde::{Deserialize, Serialize};

/// The triager's answer about one decision (§6).
///
/// **Two, and the missing third is the entire design.** There is no `Approved`, no `Ok`, no
/// `Settled`: turning something green is the owner's act and the owner's alone (§5.2), and a variant
/// here would hand back the authority §6 took away — with the compiler's blessing, which is worse
/// than a handler doing it, because nobody reviews an enum arm twice. The prohibition is written in
/// four places on purpose and this is only one of them: `0119`'s `CHECK (verdict IN ('flagged',
/// 'silenced'))` is the one that survives a caller who never reaches for this type at all.
///
/// **Neither value is a statement about the code**, and that is the sentence a reader is most likely
/// to lose. [`Self::Silenced`] does not say the decision is fine; it says the triager saw nothing
/// worth the owner's time. §6.1 refuses to let that share a colour with a stamp, and §13 rates *o
/// triador silencia o que devia mostrar* a **real** residual risk whose only mitigation is that the
/// silenced pile stays visible with the reason for each silence and the model that produced it —
/// which is what `map_triage.reason` and `map_triage.model` are, and why both are NOT NULL.
///
/// **This axis is not [`crate::map_stamp::Standing`] and not [`crate::map_join::Anchor`].** §5
/// forbids flattening them — *"achatá-las numa só punha o triador e o dono a falar pela mesma
/// boca"* — so a decision carries all three independently, and [`Self::Flagged`] feeds §5.3's `J à
/// tua espera` beside the lapsed stamps without ever becoming one.
///
/// Two wire forms, read as two, exactly as [`crate::map_stamp::Verdict`] and
/// [`crate::map_intent::Kind`] are. [`Self::as_str`] and [`Self::from_wire`] are the STORAGE form —
/// what `map_triage.verdict` holds and precisely what its CHECK admits — and the derived
/// `Serialize`/`Deserialize` are the JSON the window speaks. They spell the words alike today
/// because English happens to serve in both places, which is a convenience and not a guarantee:
/// `Kind` had to choose `b`/`c` for its column and `countable`/`character` for the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judgement {
    /// *À espera.* The triager thinks this deserves the owner's eyes, and said why.
    ///
    /// The reason is required and it is not decoration: a flag with no reason is a nag the owner
    /// cannot answer, and a nag nobody can answer is one they stop reading — which costs the same
    /// trust the silent green costs, from the other side.
    ///
    /// **The cheap failure of the two, and that asymmetry is what the parse leans on.** A wrong flag
    /// costs one unnecessary look; a wrong silence costs the very thing §1 says the map is for. So
    /// nothing here may ever default to [`Self::Silenced`].
    Flagged,
    /// *Silenciado.* The triager saw no sign of a problem. **Nobody looked.** Not green.
    ///
    /// §5.1 spells the meaning out — *sem sinal de problema* — and §6.1 spends a paragraph on why it
    /// may not share a colour with a stamp. A decision silenced here is still *nunca vista* by the
    /// owner; it has simply stopped being at the front of the queue.
    ///
    /// **Reachable only with a reason recorded, and by design it is reversible by nobody but the
    /// evidence.** §6.2 keeps the pile accessible *com a razão de cada silenciamento e o modelo que
    /// o produziu*, because *"um triador que silencia o que não devia é um bug do triador, e um bug
    /// só é corrigível se for visível."*
    Silenced,
}

impl Judgement {
    /// The storage form: what `map_triage.verdict` holds, and exactly what its CHECK admits.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flagged => "flagged",
            Self::Silenced => "silenced",
        }
    }

    /// The two, and nothing else.
    ///
    /// `Option` rather than a fallback, following [`crate::map_stamp::Verdict::from_wire`] and
    /// [`crate::map_intent::Kind::from_wire`]. Neither of the two is a safe default: guessing
    /// [`Self::Silenced`] would let a word nobody can read clear a decision out of the owner's
    /// queue, which is §13's residual risk arriving through a parser instead of through a model;
    /// guessing [`Self::Flagged`] would invent an alarm the reason column cannot explain. A
    /// judgement that cannot be read is therefore no judgement, which leaves the decision *not
    /// looked at* — visible debt, and the one answer that claims nothing.
    ///
    /// Named after the pair in `map_intent` and `map_stamp` rather than `FromStr`, because the
    /// codebase already has two of these and a third spelling would make the reader check which is
    /// which.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "flagged" => Some(Self::Flagged),
            "silenced" => Some(Self::Silenced),
            _ => None,
        }
    }
}
