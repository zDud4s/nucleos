Search tactics for code and library research: repo first, then memory, then web; compare fairly; stop when answered.

## 1. Start inside the project
- Glob and Grep the repo for the symbol, module, error text, or config key.
- Read lockfiles for exact versions (Cargo.lock, package-lock.json or equivalent, go.sum). Record them.
- Read README and docs in the repo before going outside.
- Vendored or downloaded dependency source, if present, beats any web page.

## 2. Memory recall
- Query by concrete nouns: crate or module names, exact error text, decision topics.
- Treat results as dated notes, not facts. Check the date and re-verify against the current code when it matters.
- Never copy memory contents into web queries.

## 3. Web search
- Exact error strings in quotes.
- API or function name plus version: `"fn_name" 2.3`.
- `site:` restricted to the official domain or github.com.
- Changelog query: `<lib> <version> changelog breaking`, `<lib> migration guide <old> to <new>`.
- Issue tracker query: `site:github.com/<org>/<repo> "<error or symptom>"`.
- Fetch the primary page. Do not rely on search snippets.
- For GitHub, prefer raw source at a tag or commit over rendered blog summaries.
- Never put secrets, tokens, internal hostnames, or project-private text in queries.

## 4. Prior art
- Search for the problem statement, then for the names of each competing approach.
- Read each project's own docs and code, not comparison posts.
- Use secondary write-ups only to discover candidates.

## 5. Comparing options
Use identical criteria for every option and put a small table in Synthesis:

| Option | License | Maintenance | API fit | Version compat | Size/complexity | Known issues |
|---|---|---|---|---|---|---|

Fill each cell from a source and note the source. Mark unknown cells "unknown", not a guess.

## 6. Budget and logging
- Stop once the question is answered with support. Do not browse for completeness.
- Log each source as you go: URL plus date. Sources must be complete at the end.
- Note every search that returned nothing useful (query and where) for Gaps.
- Separate what the sources state from your inference, and label inference as such.

## Output shape
`## Synthesis` (answer, versions, table if comparing), `## Sources` (URL + date), `## Gaps` (unanswered, conflicting, undated, empty searches).
