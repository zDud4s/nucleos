//! The prices half of `scripts/refresh-models.py`, ported line for line and kept pure.
//!
//! Two bodies come in (Anthropic's pricing page as markdown, OpenRouter's `/api/v1/models` as JSON)
//! and two YAML blocks go out (`prices_as_of` and `model_prices`), rendered to the byte so that a
//! file the daemon refreshes and a file the script refreshes are the same file: the owner can run
//! either, in any order, and the diff between them is empty. That is why the odd corners are kept
//! rather than tidied: Python's float `repr` (`1e-05`, `10.0`), its `round(x, 6)`, its `strip()` and
//! `splitlines()`. Each of them shows in the rendered text, and each is a diff if it drifts.
//!
//! What is not ported is the script's I/O: fetching is the caller's, and every complaint the script
//! prints comes back as a line instead.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::config::{FastPrice, ModelPrice, PriceTier, normalise_model_id};

/// Anthropic's pricing page, served as markdown. Keyless.
pub const PRICING_DOC: &str = "https://platform.claude.com/docs/en/about-claude/pricing.md";

/// OpenRouter's public catalogue, the same endpoint `capabilities.rs::discover_openrouter` reads
/// for a model's context window. Keyless.
pub const OPENROUTER_MODELS: &str = "https://openrouter.ai/api/v1/models";

/// The columns the pricing table must have, in order, after `Model`. A rename is a format change:
/// the page is refused whole rather than read by position and priced from the wrong column.
const PRICING_COLUMNS: [&str; 5] = [
    "Base input tokens",
    "5m cache writes",
    "1h cache writes",
    "Cache hits and refreshes",
    "Output tokens",
];

/// The rate fields of a price and of a tier, in the order every rendered row writes them.
const PRICE_FIELDS: [&str; 5] = [
    "input",
    "output",
    "cache_read",
    "cache_write",
    "cache_write_1h",
];

/// OpenRouter's name for each rate, and the field it lands in.
const OPENROUTER_PRICE_FIELDS: [(&str, &str); 5] = [
    ("prompt", "input"),
    ("completion", "output"),
    ("input_cache_read", "cache_read"),
    ("input_cache_write", "cache_write"),
    ("input_cache_write_1h", "cache_write_1h"),
];

/// The banner above `prices_as_of`. Portuguese because the file has always carried it in
/// Portuguese: it is quoted here verbatim, and a re-worded banner would be a diff on every refresh.
/// It still names the script, because the script still writes the same block.
pub const PRICE_BANNER: &str = r#"# ─────────────────────────────────────────────────────────────
# Escrito por `scripts/refresh-models.py`: preços publicados pelos fornecedores, USD por milhão de
# tokens, lidos em `prices_as_of`. O daemon usa-os só para o custo ESTIMADO do devtime; nunca é a
# fatura. Um modelo sem preço legível fica com `null` e o script sai com 1. Um preço por tamanho do
# prompt (Haiku 5.5, muitos `openai/*`) vai em `tiers`: cada escalão vale a partir de
# `min_prompt_tokens` tokens de prompt e traz só as tarifas que muda. `fast` é a tarifa do fast mode
# (as da cache escalam com ela) e `geo_us_mult` o multiplicador de `inference_geo: "us"`; um modelo
# sem eles deixa esses pedidos sem preço. Um preço por hora do dia não é escrito. Editar à mão é
# desfeito na próxima passagem.
# ─────────────────────────────────────────────────────────────"#;

/// The two blocks of `nucleos-models.yaml` a refresh owns, rendered and ready for
/// [`splice_price_blocks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceBlocks {
    /// The banner plus `prices_as_of: "<date>"`.
    pub prices_as_of: String,
    /// `model_prices:` and one flow-style row per model, ids sorted.
    pub model_prices: String,
}

macro_rules! pattern {
    ($name:ident, $regex:literal) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($regex).expect(concat!("the pattern ", stringify!($name))));
    };
}

pattern!(NON_SLUG, r"[^a-z0-9]+");
pattern!(SUPERSCRIPT, r"<sup>.*?</sup>");
pattern!(
    PER_MILLION,
    r"^\$([0-9]+(?:\.[0-9]+)?)(?:\s*USD)?\s*/\s*MTok$"
);
pattern!(RETIRED_LINK, r"\s*\(\[.*?\]\(.*?\)\)");
pattern!(TIER_PHRASE, r"\s*\(for prompts [^)]*\)");
pattern!(
    TIER_BOUND,
    r"^\(for prompts (up to|over) ([0-9][0-9,]*) tokens\)$"
);
pattern!(NEXT_HEADING, r"\n#{2,3} ");
pattern!(MODEL_VERSION, r"^claude-[a-z]+-([0-9]+)(?:-([0-9]+))?$");
pattern!(GEO_SINCE, r"For Claude ([0-9]+)\.([0-9]+) and later models");
pattern!(
    GEO_MULT,
    r"([0-9]+(?:\.[0-9]+)?)x multiplier on all token pricing categories"
);

// ------------------------------------------------------------------------ python's text --

/// Python's `str.isspace`: Rust's `char::is_whitespace` plus the four ASCII separators
/// (`\x1c`..`\x1f`), which Python counts as space and Rust does not.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

fn py_rstrip(text: &str) -> &str {
    text.trim_end_matches(is_py_space)
}

/// Python's `str.splitlines()`: every line boundary Python knows (not only `\n`), a `\r\n` pair as
/// one, and no empty last line for a text that ends in a newline.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (at, c) in text.char_indices() {
        if at < start {
            continue; // the `\n` of a `\r\n`, already taken with its `\r`
        }
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..at]);
        let mut end = at + c.len_utf8();
        if c == '\r' && text[end..].starts_with('\n') {
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `repr(float)`, which is what the script writes every price with: the shortest digits
/// that read back to the same float, a `.0` on a whole number (`10.0`), and an exponent of at least
/// two digits once the number leaves `1e-4 <= |x| < 1e16` (`1e-05`, `1e+16`). Rust's `{}` writes
/// `10` and `0.00001`, and its `{:?}` writes `1e-5`; each would be a diff on every refresh.
fn py_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        let infinity = if value > 0.0 { "inf" } else { "-inf" };
        return infinity.to_string();
    }
    if value == 0.0 {
        let zero = if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        };
        return zero.to_string();
    }
    // `{:e}` gives the same shortest round-trip digits Python's repr chooses, in a fixed shape.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` always writes an exponent");
    let exponent: i32 = exponent.parse().expect("`{:e}` writes a decimal exponent");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if !(-4..16).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{first}{fraction}e{exponent_sign}{:02}",
            exponent.unsigned_abs()
        )
    } else if exponent >= 0 {
        let point = exponent as usize + 1;
        if digits.len() > point {
            format!("{}.{}", &digits[..point], &digits[point..])
        } else {
            format!("{digits}{}.0", "0".repeat(point - digits.len()))
        }
    } else {
        format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    };
    format!("{sign}{body}")
}

/// Python's `repr(str)`, as far as a message quoting a table cell needs it.
fn py_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str(list_of_ints)`: `[]`, `[100001]`, `[1, 2]`.
fn py_int_list(values: &[u64]) -> String {
    let inner: Vec<String> = values.iter().map(u64::to_string).collect();
    format!("[{}]", inner.join(", "))
}

/// Python's `float(str)`: surrounding space ignored, `_` allowed between digits, `inf` and `nan`
/// read (and refused later by the caller, as the script refuses them).
fn py_float(text: &str) -> Option<f64> {
    let text = py_strip(text);
    if !text.contains('_') {
        return text.parse().ok();
    }
    let chars: Vec<char> = text.chars().collect();
    let between_digits = |at: usize| {
        at > 0
            && chars[at - 1].is_ascii_digit()
            && chars.get(at + 1).is_some_and(char::is_ascii_digit)
    };
    if (0..chars.len()).any(|at| chars[at] == '_' && !between_digits(at)) {
        return None;
    }
    text.replace('_', "").parse().ok()
}

/// Python's truthiness of a JSON value, which is what `if pricing.get("overrides"):` tests.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

// ---------------------------------------------------------------------------- rates --

/// A price's or a tier's five rates, in [`PRICE_FIELDS`] order.
type Rates = [Option<f64>; 5];

fn price_rates(price: &ModelPrice) -> Rates {
    [
        price.input,
        price.output,
        price.cache_read,
        price.cache_write,
        price.cache_write_1h,
    ]
}

fn tier_rates(tier: &PriceTier) -> Rates {
    [
        tier.input,
        tier.output,
        tier.cache_read,
        tier.cache_write,
        tier.cache_write_1h,
    ]
}

fn set_tier_rate(tier: &mut PriceTier, field: &str, rate: f64) {
    let slot = match field {
        "input" => &mut tier.input,
        "output" => &mut tier.output,
        "cache_read" => &mut tier.cache_read,
        "cache_write" => &mut tier.cache_write,
        "cache_write_1h" => &mut tier.cache_write_1h,
        other => unreachable!("{other} is not a price field"),
    };
    *slot = Some(rate);
}

// ------------------------------------------------------------------------ anthropic --

/// `Claude Opus 4.7` -> `claude-opus-4-7`, which is how the docs spell the same model as an id.
fn slug(display: &str) -> String {
    let lower = py_strip(display).to_lowercase();
    NON_SLUG
        .replace_all(&lower, "-")
        .trim_matches('-')
        .to_string()
}

/// A markdown table as `(first cell, rest of the row)` pairs, in page order, the first row of a
/// repeated key winning (the script's `dict.setdefault`). The header row keeps its own key
/// (`Model`), so a caller reads the column names out of the same structure as the values.
type Rows = Vec<(String, Vec<String>)>;

fn table_rows(markdown: &str) -> Rows {
    let unmark = |cell: &str| -> String {
        let bare: String = cell.chars().filter(|c| !matches!(c, '*' | '`')).collect();
        py_strip(&bare).to_string()
    };
    let mut rows: Rows = Vec::new();
    for line in py_splitlines(markdown) {
        let line = py_strip(line);
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(py_strip).collect();
        if cells
            .iter()
            .all(|cell| cell.chars().all(|c| matches!(c, '-' | ':' | ' ')))
        {
            continue; // the ---|--- separator
        }
        let key = unmark(cells[0]);
        if rows.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        rows.push((key, cells[1..].iter().map(|&cell| unmark(cell)).collect()));
    }
    rows
}

fn header_is(rows: &Rows, columns: &[&str]) -> bool {
    rows.iter()
        .find(|(key, _)| key == "Model")
        .is_some_and(|(_, cells)| cells.iter().map(String::as_str).eq(columns.iter().copied()))
}

/// `$0.25 / MTok<sup>1</sup>` or `$8 USD / MTok` -> the number. Anything else -> `None`, never a
/// guess. A cell the row does not have is `None` too (the script would have crashed on it).
fn per_million(cell: Option<&String>) -> Option<f64> {
    let without_notes = SUPERSCRIPT.replace_all(cell?, "");
    let found = PER_MILLION.captures(py_strip(&without_notes))?;
    found[1].parse().ok()
}

/// `(prices, problems)` from the pricing page, or `None` when its shape changed.
///
/// A row whose cells do not all parse is written with every price `None` and adds a problem. A
/// model the page prices by prompt length ("for prompts up to N tokens" / "over N tokens") is one
/// entry: the "up to" row is its base, and each "over N" row a tier from N + 1 prompt tokens on —
/// the tier applies to the whole request once its prompt passes N.
fn anthropic_prices(markdown: &str) -> Option<(BTreeMap<String, ModelPrice>, Vec<String>)> {
    let start = markdown.find("## Model pricing")?;
    let stop = markdown[start + 1..]
        .find("\n## ")
        .map_or(markdown.len(), |at| start + 1 + at);
    let rows = table_rows(&markdown[start..stop]);
    if !header_is(&rows, &PRICING_COLUMNS) {
        return None;
    }
    let mut prices: BTreeMap<String, ModelPrice> = BTreeMap::new();
    let mut tiers: Vec<(String, Vec<PriceTier>)> = Vec::new();
    let mut unreadable: BTreeSet<String> = BTreeSet::new();
    let mut problems: Vec<String> = Vec::new();
    for (name, cells) in &rows {
        if name == "Model" {
            continue;
        }
        let without_link = RETIRED_LINK.replace_all(name, ""); // "([retired…](…))"
        let bare = py_strip(&without_link);
        let tier = TIER_PHRASE.find(bare);
        let model_id = normalise_model_id(&slug(tier.map_or(bare, |found| &bare[..found.start()])));
        let [base, write_5m, write_1h, hits, out] =
            [0, 1, 2, 3, 4].map(|column| per_million(cells.get(column)));
        if base.is_none() || write_5m.is_none() || hits.is_none() || out.is_none() {
            problems.push(format!(
                "anthropic: could not read the price of {model_id} — written without price"
            ));
            unreadable.insert(model_id);
            continue;
        }
        let base_price = ModelPrice {
            input: base,
            output: out,
            cache_read: hits,
            cache_write: write_5m,
            cache_write_1h: write_1h,
            source: Some("anthropic".to_string()),
            ..ModelPrice::default()
        };
        let Some(tier) = tier else {
            prices.insert(model_id, base_price);
            continue;
        };
        let phrase = py_strip(tier.as_str());
        let bound = TIER_BOUND.captures(phrase).and_then(|found| {
            if &found[1] == "up to" {
                return Some(None);
            }
            // A threshold too large for a u64 is read as a phrase it cannot read; Python's ints
            // would have taken it, but no prompt is ever that long.
            let over: u64 = found[2].replace(',', "").parse().ok()?;
            Some(Some(over.checked_add(1)?))
        });
        match bound {
            None => {
                problems.push(format!(
                    "anthropic: could not read the tier {} of {model_id} — written without price",
                    py_str_repr(phrase)
                ));
                unreadable.insert(model_id);
            }
            Some(None) => {
                prices.insert(model_id, base_price);
            }
            Some(Some(threshold)) => {
                let tier = PriceTier {
                    min_prompt_tokens: threshold,
                    input: base,
                    output: out,
                    cache_read: hits,
                    cache_write: write_5m,
                    cache_write_1h: write_1h,
                };
                match tiers.iter_mut().find(|(id, _)| *id == model_id) {
                    Some((_, above)) => above.push(tier),
                    None => tiers.push((model_id, vec![tier])),
                }
            }
        }
    }
    for (model_id, mut above) in tiers {
        if !prices.contains_key(&model_id) {
            problems.push(format!(
                "anthropic: {model_id} has a tier but no base price — written without price"
            ));
            unreadable.insert(model_id);
        } else if !unreadable.contains(&model_id) {
            above.sort_by_key(|tier| tier.min_prompt_tokens);
            if let Some(price) = prices.get_mut(&model_id) {
                price.tiers = above;
            }
        }
    }
    for model_id in unreadable {
        prices.insert(
            model_id,
            ModelPrice {
                source: Some("anthropic".to_string()),
                ..ModelPrice::default()
            },
        );
    }
    problems.extend(anthropic_fast_prices(markdown, &mut prices));
    problems.extend(anthropic_geo_us(markdown, &mut prices));
    Some((prices, problems))
}

/// The text under one `### heading` of the page, up to the next heading of level two or three.
fn feature_section<'a>(markdown: &'a str, heading: &str) -> Option<&'a str> {
    let start = markdown.find(&format!("### {heading}"))?;
    Some(match NEXT_HEADING.find(&markdown[start + 4..]) {
        Some(following) => &markdown[start..start + 4 + following.start()],
        None => &markdown[start..],
    })
}

/// Write each model's `fast` rates from "Fast mode pricing" into `prices`; return the problems.
///
/// A row may name several models ("Claude Opus 5 / Claude Opus 4.8"). Without a readable rate a
/// model gets no `fast` at all, which the daemon reads as "a fast request is unpriced" — never the
/// standard rate, which would undercount it by the fast premium.
fn anthropic_fast_prices(markdown: &str, prices: &mut BTreeMap<String, ModelPrice>) -> Vec<String> {
    let rows = feature_section(markdown, "Fast mode pricing")
        .map(table_rows)
        .unwrap_or_default();
    if !header_is(&rows, &["Input", "Output"]) {
        return vec![
            "anthropic: no readable fast mode table — fast requests stay unpriced".to_string(),
        ];
    }
    let mut problems = Vec::new();
    for (name, cells) in &rows {
        if name == "Model" {
            continue;
        }
        let (rate_in, rate_out) = (per_million(cells.first()), per_million(cells.get(1)));
        for display in name.split(" / ") {
            let model_id = normalise_model_id(&slug(display));
            let (Some(input), Some(output)) = (rate_in, rate_out) else {
                problems.push(format!(
                    "anthropic: could not read the fast price of {model_id} — fast stays unpriced"
                ));
                continue;
            };
            match prices.get_mut(&model_id) {
                None => problems.push(format!(
                    "anthropic: fast mode prices {model_id}, which has no base price — not written"
                )),
                // An unreadable base is already a problem of its own.
                Some(price) if price.input.is_some() => {
                    price.fast = Some(FastPrice { input, output });
                }
                Some(_) => {}
            }
        }
    }
    problems
}

/// `claude-opus-4-6` -> (4, 6), `claude-fable-5` -> (5, 0); any other shape -> `None`.
fn model_version(model_id: &str) -> Option<(u64, u64)> {
    let found = MODEL_VERSION.captures(model_id)?;
    let major = found[1].parse().ok()?;
    let minor = match found.get(2) {
        Some(minor) => minor.as_str().parse().ok()?,
        None => 0,
    };
    Some((major, minor))
}

/// Write `geo_us_mult` from "Data residency pricing" into `prices`; return the problems.
///
/// The page states it once for a range of models ("For Claude 4.6 and later models, ... a 1.1x
/// multiplier on all token pricing categories"), so it goes on every priced model of that version
/// or later. Older models reject `inference_geo` outright (400), so they never need it.
fn anthropic_geo_us(markdown: &str, prices: &mut BTreeMap<String, ModelPrice>) -> Vec<String> {
    let section = feature_section(markdown, "Data residency pricing").unwrap_or("");
    let since = GEO_SINCE
        .captures(section)
        .and_then(|found| Some((found[1].parse::<u64>().ok()?, found[2].parse::<u64>().ok()?)));
    let mult = GEO_MULT
        .captures(section)
        .and_then(|found| found[1].parse::<f64>().ok());
    let (Some(first), Some(mult)) = (since, mult) else {
        return vec![
            "anthropic: no readable data residency multiplier — US-only requests stay unpriced"
                .to_string(),
        ];
    };
    for (model_id, price) in prices.iter_mut() {
        if model_version(model_id).is_some_and(|version| version >= first) && price.input.is_some()
        {
            price.geo_us_mult = Some(mult);
        }
    }
    Vec::new()
}

// ----------------------------------------------------------------------- openrouter --

/// Every model OpenRouter serves, keyed by id; `None` when the body does not parse or names no
/// model.
///
/// `None` and an empty map are deliberately not the same answer: a catalogue that came back
/// readable but EMPTY must not reach a caller as a fact, or every hosted price would vanish in one
/// pass — the loudest possible way to be wrong about a network hiccup.
pub fn parse_openrouter_catalogue(body: &str) -> Option<BTreeMap<String, Value>> {
    read_catalogue(body).ok()
}

/// [`parse_openrouter_catalogue`], keeping the reason for the refusal.
fn read_catalogue(body: &str) -> Result<BTreeMap<String, Value>, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| {
        format!("the OpenRouter catalogue did not parse ({error}) — prices unchanged")
    })?;
    let mut served = BTreeMap::new();
    for entry in payload
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = entry.get("id").and_then(Value::as_str)
            && !id.is_empty()
        {
            // A repeated id keeps its last entry, as the script's dict comprehension does.
            served.insert(id.to_string(), entry.clone());
        }
    }
    if served.is_empty() {
        return Err("the OpenRouter catalogue named no models — prices unchanged".to_string());
    }
    Ok(served)
}

/// OpenRouter's per-token USD string -> USD per million tokens; anything unreadable -> `None`.
///
/// Rounded to six decimals the way Python's `round(x, 6)` does — correctly rounded through the
/// decimal text and read back — so that `0.0000003` per token is written `0.3`, not
/// `0.30000000000000004`.
fn per_token_to_million(raw: Option<&Value>) -> Option<f64> {
    let value = match raw? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => py_float(text)?,
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    if value < 0.0 || value.is_nan() || value == f64::INFINITY {
        return None;
    }
    format!("{:.6}", value * 1_000_000.0).parse().ok()
}

/// `pricing.overrides` as the daemon's `tiers`, or `None` when any override is one it cannot price.
///
/// Only the prompt-length shape is understood: `min_prompt_tokens` plus rates. OpenRouter's N means
/// "over N", and the daemon's tier starts AT its threshold, so N is written as N + 1. An override
/// keyed on the time of day (`utc_days` / `utc_start` / `utc_end`), or on anything else, makes the
/// whole model unsupported: its price depends on when the request ran. A tier carries only the
/// rates its override names; the rest stay the base ones.
fn openrouter_tiers(overrides: &Value) -> Option<Vec<PriceTier>> {
    let mut tiers = Vec::new();
    for override_ in overrides.as_array()? {
        let override_ = override_.as_object()?;
        let known = |key: &String| {
            key == "min_prompt_tokens"
                || OPENROUTER_PRICE_FIELDS.iter().any(|(name, _)| key == name)
        };
        if !override_.keys().all(known) {
            return None;
        }
        let threshold = override_.get("min_prompt_tokens")?.as_u64()?;
        let mut tier = PriceTier {
            min_prompt_tokens: threshold.checked_add(1)?,
            ..PriceTier::default()
        };
        for (name, field) in OPENROUTER_PRICE_FIELDS {
            if let Some(raw) = override_.get(name) {
                set_tier_rate(&mut tier, field, per_token_to_million(Some(raw))?);
            }
        }
        tiers.push(tier);
    }
    tiers.sort_by_key(|tier| tier.min_prompt_tokens);
    Some(tiers)
}

/// What [`openrouter_prices`] returns: `(openai, unsupported, anthropic)`.
type OpenRouterPrices = (
    BTreeMap<String, ModelPrice>,
    BTreeSet<String>,
    BTreeMap<String, ModelPrice>,
);

/// `(openai, unsupported, anthropic)` from the catalogue.
///
/// `openai` prices every `openai/*` id (no `:variant`) under its bare, normalised id. `anthropic`
/// is the same for `anthropic/*`, used only to cross-check the vendor page and never written.
/// Overrides by prompt length become `tiers`; an `openai/*` model with any override
/// [`openrouter_tiers`] cannot read gets no price and is named in `unsupported`.
fn openrouter_prices(served: &BTreeMap<String, Value>) -> OpenRouterPrices {
    let mut openai = BTreeMap::new();
    let mut anthropic = BTreeMap::new();
    let mut unsupported = BTreeSet::new();
    let no_pricing = serde_json::Map::new();
    for (model_id, entry) in served {
        let (vendor, bare) = model_id.split_once('/').unwrap_or((model_id.as_str(), ""));
        if !matches!(vendor, "openai" | "anthropic") || bare.contains(':') || bare.is_empty() {
            continue;
        }
        let pricing = entry
            .get("pricing")
            .and_then(Value::as_object)
            .unwrap_or(&no_pricing);
        let key = normalise_model_id(bare);
        let rate = |name: &str| per_token_to_million(pricing.get(name));
        let mut row = ModelPrice {
            input: rate("prompt"),
            output: rate("completion"),
            cache_read: rate("input_cache_read"),
            cache_write: rate("input_cache_write"),
            cache_write_1h: rate("input_cache_write_1h"),
            ..ModelPrice::default()
        };
        if let Some(overrides) = pricing.get("overrides")
            && py_truthy(overrides)
        {
            let Some(tiers) = openrouter_tiers(overrides) else {
                if vendor == "openai" {
                    unsupported.insert(key);
                }
                continue;
            };
            row.tiers = tiers;
        }
        if vendor == "openai" {
            row.source = Some("openrouter".to_string());
            openai.insert(key, row);
        } else {
            anthropic.insert(key, row);
        }
    }
    (openai, unsupported, anthropic)
}

/// The vendor's prices, unchanged, and one warning per field that differs from OpenRouter by more
/// than 1%, plus one when the two start a model's tiers at different prompt lengths.
fn crosscheck(
    vendor: BTreeMap<String, ModelPrice>,
    other: &BTreeMap<String, ModelPrice>,
) -> (BTreeMap<String, ModelPrice>, Vec<String>) {
    let mut warnings = Vec::new();
    let absent = ModelPrice::default();
    for (model_id, mine) in &vendor {
        let theirs = other.get(model_id);
        if let Some(theirs) = theirs {
            let starts = |price: &ModelPrice| -> Vec<u64> {
                price
                    .tiers
                    .iter()
                    .map(|tier| tier.min_prompt_tokens)
                    .collect()
            };
            let (ours, openrouter) = (starts(mine), starts(theirs));
            if ours != openrouter {
                warnings.push(format!(
                    "{model_id} tiers: anthropic from {} vs openrouter from {} prompt tokens — keeping anthropic's",
                    py_int_list(&ours),
                    py_int_list(&openrouter)
                ));
            }
        }
        let theirs = theirs.unwrap_or(&absent);
        for ((field, a), b) in PRICE_FIELDS
            .iter()
            .zip(price_rates(mine))
            .zip(price_rates(theirs))
        {
            let (Some(a), Some(b)) = (a, b) else {
                continue;
            };
            if a == 0.0 {
                continue;
            }
            if (a - b).abs() / a > 0.01 {
                warnings.push(format!(
                    "{model_id} {field}: anthropic {} vs openrouter {} — keeping anthropic's",
                    py_float_repr(a),
                    py_float_repr(b)
                ));
            }
        }
    }
    (vendor, warnings)
}

// ------------------------------------------------------------------------ rendering --

fn render_number(value: Option<f64>) -> String {
    value.map_or_else(|| "null".to_string(), py_float_repr)
}

/// One tier, flow-style: its threshold, then only the rates it carries (absent = the base rate).
fn render_tier(tier: &PriceTier) -> String {
    let mut parts = vec![format!("min_prompt_tokens: {}", tier.min_prompt_tokens)];
    for (field, rate) in PRICE_FIELDS.iter().zip(tier_rates(tier)) {
        if let Some(rate) = rate {
            parts.push(format!("{field}: {}", py_float_repr(rate)));
        }
    }
    format!("{{ {} }}", parts.join(", "))
}

/// One `model_prices` row: the five rates always (an unreadable one as an explicit `null`), then
/// `tiers`, `fast` and `geo_us_mult` when the model has them, then `source`.
fn render_price_row(model_id: &str, price: &ModelPrice) -> String {
    let mut fields: Vec<String> = PRICE_FIELDS
        .iter()
        .zip(price_rates(price))
        .map(|(field, rate)| format!("{field}: {}", render_number(rate)))
        .collect();
    if !price.tiers.is_empty() {
        let tiers: Vec<String> = price.tiers.iter().map(render_tier).collect();
        fields.push(format!("tiers: [{}]", tiers.join(", ")));
    }
    if let Some(fast) = &price.fast {
        fields.push(format!(
            "fast: {{ input: {}, output: {} }}",
            py_float_repr(fast.input),
            py_float_repr(fast.output)
        ));
    }
    if let Some(mult) = price.geo_us_mult {
        fields.push(format!("geo_us_mult: {}", py_float_repr(mult)));
    }
    format!(
        "  {model_id}: {{ {}, source: {} }}",
        fields.join(", "),
        price.source.as_deref().unwrap_or("")
    )
}

/// The two owned price keys as blocks, ids sorted so a re-run with the same prices is a no-op diff.
fn render_prices(prices: &BTreeMap<String, ModelPrice>, as_of: &str) -> PriceBlocks {
    let mut model_prices = vec!["model_prices:".to_string()];
    model_prices.extend(prices.iter().map(|(id, price)| render_price_row(id, price)));
    PriceBlocks {
        prices_as_of: format!("{PRICE_BANNER}\nprices_as_of: \"{as_of}\""),
        model_prices: model_prices.join("\n"),
    }
}

// ------------------------------------------------------------------------- splicing --

/// Replace the `key` block, or append it. Everything else is left untouched.
///
/// A surgical text edit and not a load-and-dump: this file is mostly prose, in Portuguese,
/// explaining what each key costs, and a YAML round trip would return it with every comment gone.
/// The banner above the block is taken with it, so a rewrite does not stack banners.
fn splice(text: &str, key: &str, block: &str) -> String {
    let lines = py_splitlines(text);
    let opening = format!("{key}:");
    let Some(start) = lines.iter().position(|line| line.starts_with(&opening)) else {
        let joined = lines.join("\n");
        return format!("{}\n\n{block}\n", py_rstrip(&joined));
    };

    // Back up over the banner written last time, and over the blank line above it.
    let mut head = start;
    while head > 0 && (lines[head - 1].starts_with('#') || py_strip(lines[head - 1]).is_empty()) {
        head -= 1;
    }

    // Forward to the next column-zero line, key OR comment: the rows of this block are indented, and
    // a comment in column zero is the banner of the NEXT key (the script's `assistant_choices`
    // banner, say), which is not ours to take with us.
    let mut tail = start + 1;
    while tail < lines.len()
        && !lines[tail].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '#')
    {
        tail += 1;
    }

    let before = lines[..head].join("\n");
    let after = lines[tail..].join("\n");
    let parts: Vec<&str> = [py_rstrip(&before), block, after.trim_start_matches('\n')]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    format!("{}\n", py_rstrip(&parts.join("\n\n")))
}

/// `text` with both price blocks replaced (or appended), `prices_as_of` first and `model_prices`
/// second — the order the script splices them in, which decides where they land in a file that
/// has neither yet.
pub fn splice_price_blocks(text: &str, blocks: &PriceBlocks) -> String {
    let text = splice(text, "prices_as_of", &blocks.prices_as_of);
    splice(&text, "model_prices", &blocks.model_prices)
}

/// The script's `refresh_prices`, given the two bodies it would have fetched (`None` for a source
/// that could not be read) and today's date as `YYYY-MM-DD`.
///
/// The blocks are `None` when either source could not be read or changed shape: the file's price
/// block is then left exactly as it was, and `prices_as_of` keeps saying how old it is. The lines
/// are everything the script would have printed about the run, in its order: the cross-check
/// warnings, the `openai/*` models priced by time of day (left out), then the problems — a model
/// written without price, a fast or residency table it could not read, a source it could not use.
/// The script exits 1 on a problem and not on a warning; a clean run returns no lines at all.
pub fn refresh_from_bodies(
    pricing_md: Option<&str>,
    openrouter_json: Option<&str>,
    today: &str,
) -> (Option<PriceBlocks>, Vec<String>) {
    let mut lines = Vec::new();
    let Some(page) = pricing_md else {
        lines.push(format!("could not read {PRICING_DOC} — prices unchanged"));
        return (None, lines);
    };
    let Some((vendor, problems)) = anthropic_prices(page) else {
        lines.push(format!(
            "{PRICING_DOC} no longer has the expected pricing table — prices unchanged"
        ));
        return (None, lines);
    };
    let Some(body) = openrouter_json else {
        lines.push(format!(
            "could not read {OPENROUTER_MODELS} — prices unchanged"
        ));
        return (None, lines);
    };
    let served = match read_catalogue(body) {
        Ok(served) => served,
        Err(reason) => {
            lines.push(reason);
            return (None, lines);
        }
    };
    let (openai, unsupported, anthropic_mirror) = openrouter_prices(&served);
    let (vendor, warnings) = crosscheck(vendor, &anthropic_mirror);
    lines.extend(warnings);
    lines.extend(unsupported.iter().map(|model_id| {
        format!(
            "{model_id} has an override by time of day (or of a shape the refresh does not read) — unsupported, written without price"
        )
    }));
    lines.extend(problems);
    // A vendor id never collides with an openai id; the vendor wins anyway.
    let mut prices = openai;
    prices.extend(vendor);
    (Some(render_prices(&prices, today)), lines)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// The pricing page fixture of `scripts/test-refresh-models.py`, verbatim.
    pub(crate) const PRICING_MD: &str = r#"---
title: Pricing
---

## Model pricing

| Model | Base input tokens | 5m cache writes | 1h cache writes | Cache hits and refreshes | Output tokens |
| :---- | :---- | :---- | :---- | :---- | :---- |
| Claude Opus 4.7 | $5 / MTok | $6.25 / MTok | $10 / MTok | $0.50 / MTok | $25 / MTok |
| Claude Sonnet 5 | $2 / MTok<sup>3</sup> | $2.50 / MTok | $4 / MTok | $0.20 / MTok | $10 / MTok<sup>3</sup> |
| Claude Opus 4.1 ([retired, except on Bedrock](https://example/deprecations)) | $15 / MTok | $18.75 / MTok | $30 / MTok | $1.50 / MTok | $75 / MTok |
| Claude Haiku 5.5 (for prompts up to 100,000 tokens) | $0.10 / MTok | $0.125 / MTok | $0.20 / MTok | $0.01 / MTok | $0.50 / MTok |
| Claude Haiku 5.5 (for prompts over 100,000 tokens) | $0.50 / MTok | $0.625 / MTok | $1 / MTok | $0.05 / MTok | $2.50 / MTok |

## Cloud platform pricing

| Concept | Details |
| --- | --- |
| **Billing unit** | CCU |

## Feature-specific pricing

### Data residency pricing

For Claude 4.6 and later models, specifying US-only inference through the `inference_geo` parameter incurs a 1.1x multiplier on all token pricing categories, including input tokens, output tokens, cache writes, and cache reads.

### Fast mode pricing

[Fast mode](https://example/fast-mode) provides faster output at premium pricing.

| Model                             | Input          | Output         |
| --------------------------------- | -------------- | -------------- |
| Claude Opus 4.7 / Claude Sonnet 5 | $10 USD / MTok | $50 USD / MTok |

### Batch processing

| Model | Batch input | Batch output |
| :---- | :---- | :---- |
| Claude Opus 4.7 | $2.50 / MTok | $12.50 / MTok |
"#;

    /// A catalogue keyed by id, the shape `parse_openrouter_catalogue` returns.
    fn served(entries: Value) -> BTreeMap<String, Value> {
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry["id"].as_str().unwrap().to_string(), entry.clone()))
            .collect()
    }

    fn anthropic(markdown: &str) -> (BTreeMap<String, ModelPrice>, Vec<String>) {
        anthropic_prices(markdown).expect("the fixture has a readable pricing table")
    }

    fn price(input: f64, output: f64, cache_read: f64, cache_write: f64) -> ModelPrice {
        ModelPrice {
            input: Some(input),
            output: Some(output),
            cache_read: Some(cache_read),
            cache_write: Some(cache_write),
            ..ModelPrice::default()
        }
    }

    // --- prices: Anthropic's page ------------------------------------------------

    #[test]
    fn pricing_markdown_fixture_parses_to_per_million_prices() {
        let (prices, _) = anthropic(PRICING_MD);
        assert_eq!(
            prices["claude-opus-4-7"],
            ModelPrice {
                input: Some(5.0),
                output: Some(25.0),
                cache_read: Some(0.5),
                cache_write: Some(6.25),
                cache_write_1h: Some(10.0),
                tiers: Vec::new(),
                fast: Some(FastPrice {
                    input: 10.0,
                    output: 50.0
                }),
                geo_us_mult: Some(1.1),
                source: Some("anthropic".to_string()),
            }
        );
    }

    #[test]
    fn a_fast_row_naming_two_models_prices_both_and_a_table_after_it_is_not_read_as_fast_rates() {
        let (prices, _) = anthropic(PRICING_MD);
        assert_eq!(
            prices["claude-sonnet-5"].fast,
            Some(FastPrice {
                input: 10.0,
                output: 50.0
            })
        );
        assert_eq!(prices["claude-opus-4-7"].fast.as_ref().unwrap().input, 10.0);
    }

    #[test]
    fn the_residency_multiplier_goes_on_4_6_and_later_only_and_an_older_model_never_gets_one() {
        let (prices, _) = anthropic(PRICING_MD);
        assert_eq!(prices["claude-haiku-5-5"].geo_us_mult, Some(1.1));
        assert_eq!(prices["claude-opus-4-1"].geo_us_mult, None);
    }

    #[test]
    fn a_model_the_fast_table_does_not_name_has_no_fast_rate() {
        let (prices, _) = anthropic(PRICING_MD);
        assert_eq!(prices["claude-haiku-5-5"].fast, None);
    }

    #[test]
    fn model_version_reads_the_familys_version_and_nothing_else() {
        let versions: Vec<_> = ["claude-opus-4-6", "claude-fable-5", "gpt-5.5"]
            .into_iter()
            .map(model_version)
            .collect();
        assert_eq!(versions, vec![Some((4, 6)), Some((5, 0)), None]);
    }

    #[test]
    fn a_fast_table_that_changed_shape_writes_no_fast_rate_and_is_a_problem() {
        let unfast = PRICING_MD.replace("| Input          | Output         |", "| In | Out |");
        let (prices, problems) = anthropic(&unfast);
        assert_eq!(prices["claude-opus-4-7"].fast, None);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_fast_rate_for_a_model_with_no_base_price_is_not_written_and_is_a_problem() {
        let orphan = PRICING_MD.replace(
            "Claude Opus 4.7 / Claude Sonnet 5 |",
            "Claude Opus 4.7 / Claude Opus 9 |",
        );
        let (prices, problems) = anthropic(&orphan);
        assert!(!prices.contains_key("claude-opus-9"));
        assert_eq!(prices["claude-opus-4-7"].fast.as_ref().unwrap().input, 10.0);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_residency_sentence_it_cannot_read_writes_no_multiplier_and_is_a_problem() {
        let reworded = PRICING_MD.replace("1.1x multiplier", "surcharge");
        let (prices, problems) = anthropic(&reworded);
        assert_eq!(prices["claude-opus-4-7"].geo_us_mult, None);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_footnote_marker_does_not_stop_a_price_from_parsing() {
        let (prices, _) = anthropic(PRICING_MD);
        assert_eq!(prices["claude-sonnet-5"].input, Some(2.0));
    }

    #[test]
    fn a_retired_models_link_is_not_part_of_its_id() {
        let (prices, _) = anthropic(PRICING_MD);
        assert!(prices.contains_key("claude-opus-4-1"));
    }

    #[test]
    fn a_model_priced_by_prompt_length_is_one_entry_the_base_tier_plus_a_tier_over_100000_tokens() {
        let (prices, problems) = anthropic(PRICING_MD);
        assert_eq!(
            prices["claude-haiku-5-5"],
            ModelPrice {
                input: Some(0.1),
                output: Some(0.5),
                cache_read: Some(0.01),
                cache_write: Some(0.125),
                cache_write_1h: Some(0.2),
                tiers: vec![PriceTier {
                    min_prompt_tokens: 100_001,
                    input: Some(0.5),
                    output: Some(2.5),
                    cache_read: Some(0.05),
                    cache_write: Some(0.625),
                    cache_write_1h: Some(1.0),
                }],
                fast: None,
                geo_us_mult: Some(1.1),
                source: Some("anthropic".to_string()),
            }
        );
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_tier_with_no_base_row_is_unreadable_written_without_price_and_a_problem() {
        let orphan: Vec<&str> = py_splitlines(PRICING_MD)
            .into_iter()
            .filter(|line| !line.contains("up to 100,000"))
            .collect();
        let (prices, problems) = anthropic(&orphan.join("\n"));
        assert_eq!(prices["claude-haiku-5-5"].input, None);
        assert!(prices["claude-haiku-5-5"].tiers.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_tier_phrase_it_cannot_read_is_never_guessed_at() {
        let odd = PRICING_MD.replace(
            "(for prompts over 100,000 tokens)",
            "(for prompts between 1 and 2 tokens)",
        );
        let (prices, problems) = anthropic(&odd);
        assert_eq!(prices["claude-haiku-5-5"].input, None);
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0],
            "anthropic: could not read the tier '(for prompts between 1 and 2 tokens)' of \
             claude-haiku-5-5 — written without price"
        );
    }

    #[test]
    fn an_unreadable_price_is_written_without_price_and_is_a_problem() {
        let broken = PRICING_MD.replace("| $25 / MTok |", "| ask sales |");
        let (prices, problems) = anthropic(&broken);
        assert_eq!(
            prices["claude-opus-4-7"],
            ModelPrice {
                source: Some("anthropic".to_string()),
                ..ModelPrice::default()
            }
        );
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_renamed_column_is_none_not_a_guess() {
        let renamed = PRICING_MD.replace("Output tokens", "Output");
        assert_eq!(anthropic_prices(&renamed), None);
    }

    #[test]
    fn a_page_with_no_model_pricing_section_is_none() {
        assert_eq!(anthropic_prices("# Pricing\n\nnothing here\n"), None);
    }

    #[test]
    fn the_price_key_drops_context_and_date_suffixes_and_dots_in_claude_ids() {
        let keys: Vec<String> = [
            "Claude-Sonnet-5[1m]",
            "claude-sonnet-5-20261001",
            "claude-opus-4.6",
            "gpt-5.5",
            "claude-x-latest",
        ]
        .into_iter()
        .map(normalise_model_id)
        .collect();
        assert_eq!(
            keys,
            [
                "claude-sonnet-5",
                "claude-sonnet-5",
                "claude-opus-4-6",
                "gpt-5.5",
                "claude-x"
            ]
        );
    }

    #[test]
    fn slug_spells_a_display_name_the_way_the_docs_spell_the_id() {
        assert_eq!(slug("  Claude Opus 4.7 "), "claude-opus-4-7");
    }

    // --- prices: OpenRouter ---------------------------------------------------------

    fn catalogue() -> BTreeMap<String, Value> {
        served(json!([
            {"id": "openai/gpt-5.5", "pricing": {"prompt": "0.00000125", "completion": "0.00001",
                                                 "input_cache_read": "0.000000125"}},
            {"id": "openai/gpt-5.5:batch", "pricing": {"prompt": "0.0000006", "completion": "0.000005"}},
            {"id": "openai/gpt-6-tiered", "pricing": {
                "prompt": "0.000002", "completion": "0.00001", "input_cache_write_1h": "0.000004",
                "overrides": [{"min_prompt_tokens": 272000, "prompt": "0.000004", "completion": "0.00002",
                               "input_cache_read": "0.0000004"}]}},
            {"id": "openai/gpt-night", "pricing": {
                "prompt": "0.000002", "completion": "0.00001",
                "overrides": [{"utc_days": [0, 1], "utc_start": "00:00", "utc_end": "06:00", "prompt": "0.000001"}]}},
            {"id": "openai/gpt-mixed", "pricing": {
                "prompt": "0.000002", "completion": "0.00001",
                "overrides": [{"min_prompt_tokens": 1000, "prompt": "0.000003"},
                              {"utc_start": "00:00", "utc_end": "06:00", "prompt": "0.000001"}]}},
            {"id": "anthropic/claude-opus-4.7", "pricing": {"prompt": "0.000005", "completion": "0.000025",
                                                            "input_cache_read": "0.0000005",
                                                            "input_cache_write": "0.00000625"}},
            {"id": "google/gemini-x", "pricing": {"prompt": "0.000001", "completion": "0.000002"}}
        ]))
    }

    #[test]
    fn openrouter_pricing_sample_converts_per_token_to_per_million() {
        let (openai, _, _) = openrouter_prices(&catalogue());
        assert_eq!(
            openai["gpt-5.5"],
            ModelPrice {
                input: Some(1.25),
                output: Some(10.0),
                cache_read: Some(0.125),
                source: Some("openrouter".to_string()),
                ..ModelPrice::default()
            }
        );
    }

    #[test]
    fn a_prompt_length_override_becomes_a_tier_carrying_only_the_rates_it_overrides_from_n_plus_1()
    {
        let (openai, _, _) = openrouter_prices(&catalogue());
        assert_eq!(
            openai["gpt-6-tiered"],
            ModelPrice {
                input: Some(2.0),
                output: Some(10.0),
                cache_write_1h: Some(4.0),
                tiers: vec![PriceTier {
                    min_prompt_tokens: 272_001,
                    input: Some(4.0),
                    output: Some(20.0),
                    cache_read: Some(0.4),
                    ..PriceTier::default()
                }],
                source: Some("openrouter".to_string()),
                ..ModelPrice::default()
            }
        );
    }

    #[test]
    fn batch_variants_and_other_vendors_are_not_priced_and_time_of_day_overrides_are_named_never_priced()
     {
        let (openai, unsupported, _) = openrouter_prices(&catalogue());
        assert_eq!(
            openai.keys().collect::<Vec<_>>(),
            ["gpt-5.5", "gpt-6-tiered"]
        );
        assert_eq!(
            unsupported.iter().collect::<Vec<_>>(),
            ["gpt-mixed", "gpt-night"]
        );
    }

    #[test]
    fn anthropic_entries_come_back_apart_normalised_for_the_cross_check_only() {
        let (_, _, mirror) = openrouter_prices(&catalogue());
        assert_eq!(mirror["claude-opus-4-7"].output, Some(25.0));
        assert_eq!(mirror["claude-opus-4-7"].source, None);
    }

    #[test]
    fn crosscheck_divergence_is_warned_and_vendor_price_wins() {
        let vendor = BTreeMap::from([(
            "claude-opus-4-7".to_string(),
            ModelPrice {
                source: Some("anthropic".to_string()),
                ..price(5.0, 25.0, 0.5, 6.25)
            },
        )]);
        let other = BTreeMap::from([("claude-opus-4-7".to_string(), price(5.2, 25.0, 0.5, 6.25))]);
        let (merged, warnings) = crosscheck(vendor, &other);
        assert_eq!(merged["claude-opus-4-7"].input, Some(5.0));
        assert_eq!(
            warnings,
            ["claude-opus-4-7 input: anthropic 5.0 vs openrouter 5.2 — keeping anthropic's"]
        );
    }

    #[test]
    fn a_divergence_under_1_percent_is_not_warned() {
        let vendor = BTreeMap::from([("claude-opus-4-7".to_string(), price(5.0, 25.0, 0.5, 6.25))]);
        let other = BTreeMap::from([(
            "claude-opus-4-7".to_string(),
            ModelPrice {
                input: Some(5.04),
                ..ModelPrice::default()
            },
        )]);
        let (_, warnings) = crosscheck(vendor, &other);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    fn tiered_from(min_prompt_tokens: u64) -> BTreeMap<String, ModelPrice> {
        BTreeMap::from([(
            "claude-haiku-5-5".to_string(),
            ModelPrice {
                input: Some(0.1),
                tiers: vec![PriceTier {
                    min_prompt_tokens,
                    ..PriceTier::default()
                }],
                ..ModelPrice::default()
            },
        )])
    }

    #[test]
    fn tiers_starting_at_the_same_prompt_length_are_not_warned() {
        let (_, warnings) = crosscheck(tiered_from(100_001), &tiered_from(100_001));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn tiers_starting_one_token_apart_are_warned_and_the_vendors_threshold_wins() {
        let (merged, warnings) = crosscheck(tiered_from(100_001), &tiered_from(100_000));
        assert_eq!(
            merged["claude-haiku-5-5"].tiers[0].min_prompt_tokens,
            100_001
        );
        assert_eq!(
            warnings,
            [
                "claude-haiku-5-5 tiers: anthropic from [100001] vs openrouter from [100000] prompt \
              tokens — keeping anthropic's"
            ]
        );
    }

    #[test]
    fn an_unreachable_catalogue_body_that_does_not_parse_is_none_and_not_an_empty_menu() {
        assert_eq!(parse_openrouter_catalogue("not json at all"), None);
    }

    #[test]
    fn a_catalogue_that_parses_to_no_models_is_a_failure_not_a_wipe() {
        assert_eq!(parse_openrouter_catalogue(r#"{"data": []}"#), None);
        assert_eq!(
            parse_openrouter_catalogue(r#"{"data": [{"id": ""}, 3]}"#),
            None
        );
    }

    #[test]
    fn a_catalogue_that_parses_comes_back_keyed_by_id() {
        let served =
            parse_openrouter_catalogue(r#"{"data": [{"id": "a/b", "name": "A: B"}]}"#).unwrap();
        assert_eq!(served.keys().collect::<Vec<_>>(), ["a/b"]);
        assert_eq!(served["a/b"]["name"], "A: B");
    }

    #[test]
    fn per_token_rates_round_to_six_decimals_the_way_python_does() {
        let million = |raw: Value| per_token_to_million(Some(&raw));
        assert_eq!(million(json!("0.0000003")), Some(0.3));
        assert_eq!(million(json!("0.00000125")), Some(1.25));
        assert_eq!(million(json!(" 0.000001 ")), Some(1.0));
        assert_eq!(million(json!(0)), Some(0.0));
        assert_eq!(million(json!("0.00000000001")), Some(1e-05));
        assert_eq!(million(json!("-0.000001")), None);
        assert_eq!(million(json!("nan")), None);
        assert_eq!(million(json!("inf")), None);
        assert_eq!(million(json!("free")), None);
        assert_eq!(million(Value::Null), None);
    }

    // --- prices: rendering and splicing ----------------------------------------------

    #[test]
    fn floats_are_written_the_way_python_repr_writes_them() {
        let cases = [
            (0.3, "0.3"),
            (3.75, "3.75"),
            (10.0, "10.0"),
            (1e-07, "1e-07"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (1e-05, "1e-05"),
            (1.5e-05, "1.5e-05"),
            (0.30000000000000004, "0.30000000000000004"),
            (123456.789, "123456.789"),
            (2.5e-10, "2.5e-10"),
            (-1.25, "-1.25"),
            (100000.0, "100000.0"),
            (1.2345678901234568e17, "1.2345678901234568e+17"),
            (0.0, "0.0"),
        ];
        for (value, python) in cases {
            assert_eq!(py_float_repr(value), python, "{value:e}");
        }
    }

    #[test]
    fn splice_replaces_model_prices_block_and_leaves_other_keys_intact() {
        let original = "local_assistant_model: \"qwen\"\n\n# banner\nassistant_choices:\n  - { id: \"opus\" }\n\n\
                        prices_as_of: \"2026-01-01\"\nmodel_prices:\n  old: { input: 1.0 }\n\nembedding_model: x\n";
        let prices = BTreeMap::from([(
            "claude-opus-4-7".to_string(),
            ModelPrice {
                source: Some("anthropic".to_string()),
                ..price(5.0, 25.0, 0.5, 6.25)
            },
        )]);
        let blocks = render_prices(&prices, "2026-10-18");
        let spliced = splice(original, "model_prices", &blocks.model_prices);
        let spliced = splice(&spliced, "prices_as_of", &blocks.prices_as_of);
        assert!(!spliced.contains("old:"));
        assert!(spliced.contains("prices_as_of: \"2026-10-18\""));
        assert!(spliced.contains("local_assistant_model: \"qwen\""));
        assert!(spliced.contains("assistant_choices:"));
        assert!(spliced.contains("embedding_model: x"));
        assert_eq!(spliced.matches("model_prices:").count(), 1);
    }

    #[test]
    fn a_price_row_is_flow_style_with_explicit_nulls() {
        let row = render_price_row(
            "gpt-5.5",
            &ModelPrice {
                input: Some(1.25),
                output: Some(10.0),
                cache_read: Some(0.125),
                source: Some("openrouter".to_string()),
                ..ModelPrice::default()
            },
        );
        assert_eq!(
            row,
            "  gpt-5.5: { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: null, \
             cache_write_1h: null, source: openrouter }"
        );
    }

    #[test]
    fn a_tiered_row_writes_its_tiers_inline_each_with_only_the_rates_it_carries() {
        let row = render_price_row(
            "claude-haiku-5-5",
            &ModelPrice {
                input: Some(0.1),
                output: Some(0.5),
                cache_read: Some(0.01),
                cache_write: Some(0.125),
                cache_write_1h: Some(0.2),
                tiers: vec![PriceTier {
                    min_prompt_tokens: 100_001,
                    input: Some(0.5),
                    output: Some(2.5),
                    ..PriceTier::default()
                }],
                source: Some("anthropic".to_string()),
                ..ModelPrice::default()
            },
        );
        assert_eq!(
            row,
            "  claude-haiku-5-5: { input: 0.1, output: 0.5, cache_read: 0.01, cache_write: 0.125, \
             cache_write_1h: 0.2, tiers: [{ min_prompt_tokens: 100001, input: 0.5, output: 2.5 }], \
             source: anthropic }"
        );
    }

    #[test]
    fn a_row_with_fast_and_residency_rates_writes_both_after_its_tiers() {
        let row = render_price_row(
            "claude-opus-5-5",
            &ModelPrice {
                cache_write_1h: Some(8.0),
                fast: Some(FastPrice {
                    input: 8.0,
                    output: 40.0,
                }),
                geo_us_mult: Some(1.1),
                source: Some("anthropic".to_string()),
                ..price(4.0, 20.0, 0.2, 5.0)
            },
        );
        assert_eq!(
            row,
            "  claude-opus-5-5: { input: 4.0, output: 20.0, cache_read: 0.2, cache_write: 5.0, \
             cache_write_1h: 8.0, fast: { input: 8.0, output: 40.0 }, geo_us_mult: 1.1, source: anthropic }"
        );
    }

    #[test]
    fn lines_split_the_way_python_splitlines_splits_them() {
        assert_eq!(py_splitlines("a\r\nb\rc\n"), ["a", "b", "c"]);
        assert_eq!(py_splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(py_splitlines("a\u{2028}b"), ["a", "b"]);
        assert!(py_splitlines("").is_empty());
    }

    #[test]
    fn splice_appends_a_key_the_file_does_not_have_yet() {
        assert_eq!(splice("a: 1\n\n\n", "b", "b: 2"), "a: 1\n\nb: 2\n");
    }

    #[test]
    fn splice_leaves_the_banner_of_the_key_after_it_in_place() {
        let text = "model_prices:\n  old: {}\n\n# menus banner\nassistant_choices:\n  - x\n";
        assert_eq!(
            splice(text, "model_prices", "model_prices:\n  new: {}"),
            "model_prices:\n  new: {}\n\n# menus banner\nassistant_choices:\n  - x\n"
        );
    }

    // --- prices: the whole refresh ----------------------------------------------------

    pub(crate) const GOOD: &str = r#"{"data": [{"id": "openai/gpt-5.5", "pricing": {"prompt": "0.00000125", "completion": "0.00001"}}]}"#;

    #[test]
    fn a_clean_read_writes_both_sources_and_does_not_complain() {
        let (blocks, lines) = refresh_from_bodies(Some(PRICING_MD), Some(GOOD), "2026-10-18");
        let blocks = blocks.unwrap();
        assert!(blocks.model_prices.contains("claude-opus-4-7:"));
        assert!(blocks.model_prices.contains("gpt-5.5:"));
        assert!(lines.is_empty(), "{lines:?}");
        assert!(
            blocks
                .prices_as_of
                .ends_with("\nprices_as_of: \"2026-10-18\"")
        );
    }

    #[test]
    fn a_clean_read_writes_the_fast_and_residency_rates() {
        let (blocks, _) = refresh_from_bodies(Some(PRICING_MD), Some(GOOD), "2026-10-18");
        assert!(
            blocks
                .unwrap()
                .model_prices
                .contains("fast: { input: 10.0, output: 50.0 }, geo_us_mult: 1.1")
        );
    }

    #[test]
    fn a_clean_read_writes_the_tiered_model_with_its_tier() {
        let (blocks, _) = refresh_from_bodies(Some(PRICING_MD), Some(GOOD), "2026-10-18");
        let model_prices = blocks.unwrap().model_prices;
        assert!(model_prices.contains("claude-haiku-5-5: { input: 0.1"));
        assert!(model_prices.contains("min_prompt_tokens: 100001"));
    }

    #[test]
    fn an_unreadable_price_is_written_without_price_and_complains() {
        let broken = PRICING_MD.replace("| $25 / MTok |", "| ask sales |");
        let (blocks, lines) = refresh_from_bodies(Some(&broken), Some(GOOD), "2026-10-18");
        assert!(
            blocks
                .unwrap()
                .model_prices
                .contains("claude-opus-4-7: { input: null")
        );
        assert!(!lines.is_empty());
    }

    #[test]
    fn a_pricing_page_format_change_writes_nothing_and_complains() {
        let renamed = PRICING_MD.replace("Output tokens", "Output");
        let (blocks, lines) = refresh_from_bodies(Some(&renamed), Some(GOOD), "2026-10-18");
        assert_eq!(blocks, None);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_unreachable_pricing_page_writes_nothing_and_complains() {
        let (blocks, lines) = refresh_from_bodies(None, Some(GOOD), "2026-10-18");
        assert_eq!(blocks, None);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_unreachable_or_empty_catalogue_writes_nothing_and_complains() {
        for body in [None, Some("not json"), Some(r#"{"data": []}"#)] {
            let (blocks, lines) = refresh_from_bodies(Some(PRICING_MD), body, "2026-10-18");
            assert_eq!(blocks, None, "{body:?}");
            assert_eq!(lines.len(), 1, "{body:?}");
        }
    }

    /// The bodies below were fed to `scripts/refresh-models.py` (its `fetch` swapped for them) and
    /// the expected texts are what it wrote, pasted unchanged: the daemon's refresh and the script
    /// must write the same file.
    const GOLDEN_CATALOGUE: &str = r#"{"data": [{"id": "openai/gpt-5.5", "pricing": {"prompt": "0.00000125", "completion": "0.00001", "input_cache_read": "0.000000125"}}, {"id": "openai/gpt-5.5:batch", "pricing": {"prompt": "0.0000006", "completion": "0.000005"}}, {"id": "openai/gpt-6-tiered", "pricing": {"prompt": "0.000002", "completion": "0.00001", "input_cache_write_1h": "0.000004", "overrides": [{"min_prompt_tokens": 272000, "prompt": "0.000004", "completion": "0.00002", "input_cache_read": "0.0000004"}]}}, {"id": "openai/gpt-night", "pricing": {"prompt": "0.000002", "completion": "0.00001", "overrides": [{"utc_days": [0, 1], "utc_start": "00:00", "utc_end": "06:00", "prompt": "0.000001"}]}}, {"id": "openai/gpt-tiny", "pricing": {"prompt": "0.0000003", "completion": "0.00000000001", "input_cache_read": 0}}, {"id": "anthropic/claude-opus-4.7", "pricing": {"prompt": "0.000005", "completion": "0.000026", "input_cache_read": "0.0000005"}}, {"id": "google/gemini-x", "pricing": {"prompt": "0.000001", "completion": "0.000002"}}]}"#;
    const GOLDEN_ORIGINAL: &str = "claude_model: opus\ncodex_model: gpt-a\n\n# banner\nassistant_choices:\n  - { id: \"opus\" }\n\n\
                                   prices_as_of: \"2026-01-01\"\nmodel_prices:\n  old: { input: 1.0 }\n\nembedding_model: x\n";
    const GOLDEN_SPLICED: &str = r#"claude_model: opus
codex_model: gpt-a

# banner
assistant_choices:
  - { id: "opus" }

# ─────────────────────────────────────────────────────────────
# Escrito por `scripts/refresh-models.py`: preços publicados pelos fornecedores, USD por milhão de
# tokens, lidos em `prices_as_of`. O daemon usa-os só para o custo ESTIMADO do devtime; nunca é a
# fatura. Um modelo sem preço legível fica com `null` e o script sai com 1. Um preço por tamanho do
# prompt (Haiku 5.5, muitos `openai/*`) vai em `tiers`: cada escalão vale a partir de
# `min_prompt_tokens` tokens de prompt e traz só as tarifas que muda. `fast` é a tarifa do fast mode
# (as da cache escalam com ela) e `geo_us_mult` o multiplicador de `inference_geo: "us"`; um modelo
# sem eles deixa esses pedidos sem preço. Um preço por hora do dia não é escrito. Editar à mão é
# desfeito na próxima passagem.
# ─────────────────────────────────────────────────────────────
prices_as_of: "2026-10-18"

model_prices:
  claude-haiku-5-5: { input: 0.1, output: 0.5, cache_read: 0.01, cache_write: 0.125, cache_write_1h: 0.2, tiers: [{ min_prompt_tokens: 100001, input: 0.5, output: 2.5, cache_read: 0.05, cache_write: 0.625, cache_write_1h: 1.0 }], geo_us_mult: 1.1, source: anthropic }
  claude-opus-4-1: { input: 15.0, output: 75.0, cache_read: 1.5, cache_write: 18.75, cache_write_1h: 30.0, source: anthropic }
  claude-opus-4-7: { input: 5.0, output: 25.0, cache_read: 0.5, cache_write: 6.25, cache_write_1h: 10.0, fast: { input: 10.0, output: 50.0 }, geo_us_mult: 1.1, source: anthropic }
  claude-sonnet-5: { input: 2.0, output: 10.0, cache_read: 0.2, cache_write: 2.5, cache_write_1h: 4.0, fast: { input: 10.0, output: 50.0 }, geo_us_mult: 1.1, source: anthropic }
  gpt-5.5: { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: null, cache_write_1h: null, source: openrouter }
  gpt-6-tiered: { input: 2.0, output: 10.0, cache_read: null, cache_write: null, cache_write_1h: 4.0, tiers: [{ min_prompt_tokens: 272001, input: 4.0, output: 20.0, cache_read: 0.4 }], source: openrouter }
  gpt-tiny: { input: 0.3, output: 1e-05, cache_read: 0.0, cache_write: null, cache_write_1h: null, source: openrouter }

embedding_model: x
"#;
    const GOLDEN_APPENDED: &str = r#"claude_model: opus
codex_model: gpt-a

# ─────────────────────────────────────────────────────────────
# Escrito por `scripts/refresh-models.py`: preços publicados pelos fornecedores, USD por milhão de
# tokens, lidos em `prices_as_of`. O daemon usa-os só para o custo ESTIMADO do devtime; nunca é a
# fatura. Um modelo sem preço legível fica com `null` e o script sai com 1. Um preço por tamanho do
# prompt (Haiku 5.5, muitos `openai/*`) vai em `tiers`: cada escalão vale a partir de
# `min_prompt_tokens` tokens de prompt e traz só as tarifas que muda. `fast` é a tarifa do fast mode
# (as da cache escalam com ela) e `geo_us_mult` o multiplicador de `inference_geo: "us"`; um modelo
# sem eles deixa esses pedidos sem preço. Um preço por hora do dia não é escrito. Editar à mão é
# desfeito na próxima passagem.
# ─────────────────────────────────────────────────────────────
prices_as_of: "2026-10-18"

model_prices:
  claude-haiku-5-5: { input: 0.1, output: 0.5, cache_read: 0.01, cache_write: 0.125, cache_write_1h: 0.2, tiers: [{ min_prompt_tokens: 100001, input: 0.5, output: 2.5, cache_read: 0.05, cache_write: 0.625, cache_write_1h: 1.0 }], geo_us_mult: 1.1, source: anthropic }
  claude-opus-4-1: { input: 15.0, output: 75.0, cache_read: 1.5, cache_write: 18.75, cache_write_1h: 30.0, source: anthropic }
  claude-opus-4-7: { input: 5.0, output: 25.0, cache_read: 0.5, cache_write: 6.25, cache_write_1h: 10.0, fast: { input: 10.0, output: 50.0 }, geo_us_mult: 1.1, source: anthropic }
  claude-sonnet-5: { input: 2.0, output: 10.0, cache_read: 0.2, cache_write: 2.5, cache_write_1h: 4.0, fast: { input: 10.0, output: 50.0 }, geo_us_mult: 1.1, source: anthropic }
  gpt-5.5: { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: null, cache_write_1h: null, source: openrouter }
  gpt-6-tiered: { input: 2.0, output: 10.0, cache_read: null, cache_write: null, cache_write_1h: 4.0, tiers: [{ min_prompt_tokens: 272001, input: 4.0, output: 20.0, cache_read: 0.4 }], source: openrouter }
  gpt-tiny: { input: 0.3, output: 1e-05, cache_read: 0.0, cache_write: null, cache_write_1h: null, source: openrouter }
"#;

    #[test]
    fn the_refresh_writes_byte_for_byte_what_the_script_writes() {
        let (blocks, lines) =
            refresh_from_bodies(Some(PRICING_MD), Some(GOLDEN_CATALOGUE), "2026-10-18");
        let blocks = blocks.unwrap();
        assert_eq!(
            splice_price_blocks(GOLDEN_ORIGINAL, &blocks),
            GOLDEN_SPLICED
        );
        assert_eq!(
            splice_price_blocks("claude_model: opus\ncodex_model: gpt-a\n", &blocks),
            GOLDEN_APPENDED
        );
        assert_eq!(
            lines,
            [
                "claude-opus-4-7 output: anthropic 25.0 vs openrouter 26.0 — keeping anthropic's",
                "gpt-night has an override by time of day (or of a shape the refresh does not read) \
                 — unsupported, written without price",
            ]
        );
    }

    #[test]
    fn a_refresh_of_a_file_already_refreshed_is_a_no_op() {
        let (blocks, _) =
            refresh_from_bodies(Some(PRICING_MD), Some(GOLDEN_CATALOGUE), "2026-10-18");
        let blocks = blocks.unwrap();
        assert_eq!(splice_price_blocks(GOLDEN_SPLICED, &blocks), GOLDEN_SPLICED);
    }

    #[test]
    fn the_spliced_file_reads_back_as_the_prices_it_was_rendered_from() {
        let (blocks, _) =
            refresh_from_bodies(Some(PRICING_MD), Some(GOLDEN_CATALOGUE), "2026-10-18");
        let text =
            splice_price_blocks("claude_model: opus\ncodex_model: gpt-a\n", &blocks.unwrap());
        let config = crate::config::parse_models_config(&text).expect("the spliced file parses");
        assert!(
            config.price_warnings.is_empty(),
            "{:?}",
            config.price_warnings
        );
        assert_eq!(config.prices_as_of.as_deref(), Some("2026-10-18"));
        let haiku = &config.model_prices["claude-haiku-5-5"];
        assert_eq!(haiku.input, Some(0.1));
        assert_eq!(
            haiku.tiers,
            [PriceTier {
                min_prompt_tokens: 100_001,
                input: Some(0.5),
                output: Some(2.5),
                cache_read: Some(0.05),
                cache_write: Some(0.625),
                cache_write_1h: Some(1.0),
            }]
        );
        assert_eq!(haiku.geo_us_mult, Some(1.1));
        let opus = &config.model_prices["claude-opus-4-7"];
        assert_eq!(
            opus.fast,
            Some(FastPrice {
                input: 10.0,
                output: 50.0
            })
        );
        assert_eq!(opus.source.as_deref(), Some("anthropic"));
        let tiny = &config.model_prices["gpt-tiny"];
        assert_eq!(
            tiny.output,
            Some(1e-05),
            "`1e-05` reads back as the float it was written from"
        );
        assert_eq!(tiny.cache_write, None);
        assert_eq!(tiny.source.as_deref(), Some("openrouter"));
        assert_eq!(config.model_prices.len(), 7);
    }
}
