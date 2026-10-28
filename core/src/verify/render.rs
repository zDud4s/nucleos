//! The text of a verification ticket: one line per unit, then its note, its unclaimed files and
//! its verdict. Read by `nucleos-core --verify` (`verify_cli`) and by the verdict prompt a woken
//! run is resumed with (`execution/runs/verify_wake.rs`).

use serde_json::Value;

/// One line per unit: its status, then its group, or its argv when it has none. A unit that was
/// red before the change says so, and the tail of a failed or errored unit is indented below it.
pub fn render_units(ticket: &Value) -> String {
    let mut out = String::new();
    let Some(units) = ticket["units"].as_array() else {
        return out;
    };
    for unit in units {
        let status = unit["status"].as_str().unwrap_or("unknown");
        let label = match unit["group"].as_str() {
            Some(group) if !group.is_empty() => group.to_string(),
            _ => unit["argv"]
                .as_array()
                .map(|argv| {
                    argv.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
        };
        out.push_str(&format!("{status:<14} {label}"));
        if let Some(already) = unit["already_failing"].as_str() {
            out.push_str(&format!(" (already failing on the target: {already})"));
        }
        out.push('\n');
        if matches!(status, "failed" | "errored")
            && let Some(tail) = unit["output_tail"].as_str()
        {
            for line in tail.lines() {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    out
}

/// The listing of a finished ticket: its units, the ticket's note and the files no unit covers
/// (each only when there is one), then the verdict and the ticket it belongs to.
pub fn render(ticket: &Value) -> String {
    let id = ticket["ticket"].as_i64().unwrap_or_default();
    let verdict = ticket["verdict"].as_str().unwrap_or("none");
    let mut out = render_units(ticket);
    if let Some(note) = ticket["note"].as_str().filter(|note| !note.is_empty()) {
        out.push_str(&format!("note: {note}\n"));
    }
    if let Some(unclaimed) = ticket["unclaimed"]
        .as_array()
        .filter(|list| !list.is_empty())
    {
        out.push_str("unclaimed (no unit covers these files):\n");
        for path in unclaimed.iter().filter_map(Value::as_str) {
            out.push_str(&format!("    {path}\n"));
        }
    }
    out.push_str(&format!("verdict: {verdict} (ticket {id})\n"));
    out
}
