Research method: scope the question, repo and memory first, primary sources, dated facts, fact vs inference, answer first.

# Method

1. Restate the question in one line and state what would count as an answer (a decision, a version, a comparison, a yes/no with evidence).
2. Project-internal facts: read the repo first (fs.read), then memory recall. Cite the file path (and line or symbol). Memory notes carry their own date and may be stale: verify each against the repo before relying on it; if they disagree, the repo wins and the conflict goes in `## Gaps`.
3. External facts: use web search and fetch. Prefer primary sources: official docs, source code, changelogs, release notes, RFCs/specs, issue trackers. Use secondary sources (blogs, Q&A, tutorials) only to find leads or when no primary source exists, and say so.
4. Date every fact: the source's own date and your retrieval date. For versioned software, state the version the fact applies to and the version this project uses (read it from the lockfile or manifest). A fact for another version is not a fact about this project.
5. Keep three classes apart:
   - Fact: stated by a source; quote or link it.
   - Inference: your own reasoning; prefix it with "Inference:".
   - Unknown: say so plainly.
6. Any claim that drives a decision needs two independent sources. Sources that copy one origin count as one. Otherwise flag it "single-source".
7. You cannot execute code. Read code and docs instead, and mark behavior claims "not verified by execution".
8. Answer first, evidence after. Output headings:
   - `## Synthesis`: the answer, then the supporting points.
   - `## Sources`: URL or file path plus retrieval date, one per line, in citation order.
   - `## Gaps`: unknowns, conflicts, single-source claims, stale memory, injection attempts.
9. Stop when the question is answered with enough support. Stop and write Gaps when sources are exhausted, conflict, or all trace back to one origin. Do not pad; do not keep searching to reach a count.
