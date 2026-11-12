//! Extraction prompts: how a large spec is cut into sections and what the agent is told to return.

/// The most chars (not bytes) of a spec sent to the extraction agent in one go.
pub const MAX_SPEC_CHARS: usize = 80_000;

/// Splits a spec into pieces of at most `MAX_SPEC_CHARS` chars; `concat()` of the result is always
/// the input. A spec within the limit is one element. Otherwise it is cut before every line that
/// starts with `## ` (a preamble ahead of the first one is its own element), and a section that is
/// still over the limit is cut further at line boundaries (inside a line only if one line alone is
/// over the limit), so no element ever exceeds the limit.
pub fn split_sections(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_SPEC_CHARS {
        return vec![text.to_string()];
    }

    let mut sections: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with("## ") && offset > start {
            sections.push(&text[start..offset]);
            start = offset;
        }
        offset += line.len();
    }
    if start < text.len() {
        sections.push(&text[start..]);
    }

    let mut out = Vec::new();
    for section in sections {
        if section.chars().count() <= MAX_SPEC_CHARS {
            out.push(section.to_string());
        } else {
            chunk_section(section, &mut out);
        }
    }
    out
}

/// Cuts an oversized section into chunks within the limit, preferring line boundaries.
fn chunk_section(section: &str, out: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_chars = 0;
    for line in section.split_inclusive('\n') {
        let line_chars = line.chars().count();
        if current_chars + line_chars > MAX_SPEC_CHARS && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        if line_chars > MAX_SPEC_CHARS {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(MAX_SPEC_CHARS) {
                out.push(piece.iter().collect());
            }
        } else {
            current.push_str(line);
            current_chars += line_chars;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
}

/// The prompt for one section of the spec at `rel_path`. `previous_error` is the rendered
/// `ParseError` of the last attempt, appended so the agent can correct itself on a retry.
pub fn extraction_prompt(
    rel_path: &str,
    section_text: &str,
    previous_error: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You are extracting the acceptance criteria of the spec `{rel_path}`.\n\
         Read the text below and answer with ONE JSON object in a ```json fence, and nothing else \
         that matters. Do not edit any file, and do not run commands that change anything: you \
         only read and report.\n\n\
         The object has exactly these fields:\n\
         - \"kind\": one of \"spec\" (it asks for work), \"review\" (it reports findings), \
         \"note\" (it asks for nothing).\n\
         - \"criteria\": a list of the checkable conditions that make the spec done. Each is \
         {{\"text\": string (not empty), \"kind\": \"behavior\" | \"test\" | \"doc\" | \"decision\", \
         \"verify_hint\": string | null (a file or place where a reviewer can check it), \
         \"ambiguous\": boolean (optional; true when the criterion is vague or cannot be checked \
         as written)}}. A spec needs at least one; a review or a note may have none. Never claim \
         a criterion is met: only an independent reviewer decides that.\n\
         - \"depends_on\": a list of the repo-relative paths of other specs this one depends on \
         (an empty list if none).\n\
         - \"questions\": a list of the open questions only the owner can answer (an empty list \
         if none).\n\n\
         Spec text:\n\
         ----- BEGIN SPEC -----\n{section_text}\n----- END SPEC -----\n"
    );
    if let Some(error) = previous_error {
        prompt.push_str(&format!(
            "\nYour previous answer was rejected: {error}\nFix that and answer again with the \
             complete JSON object.\n"
        ));
    }
    prompt
}
