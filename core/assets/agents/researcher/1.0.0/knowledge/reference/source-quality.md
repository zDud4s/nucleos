Source ladder for software research: rank sources, date claims, match versions, spot stale or spam content.

## Ladder (best to worst)
1. The code itself at the pinned version (lockfile, vendored source, git tag).
2. Official spec or RFC.
3. Official docs for the matching version.
4. Official changelog, release notes, migration guides.
5. Maintainer statements in issues, PRs, discussions.
6. Registry metadata: crates.io, npm, pkg.go.dev, PyPI. Read publish dates, yanked and deprecated flags, license, dependents.
7. Reputable secondary: conference talks, write-ups by well-known engineers.
8. Q&A answers. Check answer date, votes, and whether a comment says it broke.
9. Blogs and tutorials.
10. AI-generated or SEO content farms. Never cite as support.

When two sources conflict, the higher rung wins. Report the conflict in Gaps if it is unresolved.

## Match the version first
- Find the version the project uses: Cargo.toml and Cargo.lock, package.json and its lockfile, go.mod and go.sum.
- Docs URLs: prefer version-pinned (`/1.2.3/`, `/v18/`, a tag path) over `latest` or `stable`. A `latest` page describes today's release, not necessarily the project's.
- If only `latest` docs exist, check the changelog between the pinned version and latest for the API in question.
- State the version each claim applies to in Synthesis.

## Dating a claim
- Page footer or "last updated" field; the date in the URL.
- For repos: commit date of the line (git blame or the GitHub history view), tag or release date.
- Registry publish date of the version.
- Wayback Machine is a hint only, not proof of when content was written.
- Sources lines always carry the retrieval date (the required line format has no "undated"). If the document's own date cannot be found, write "undated" in Synthesis and flag it in Gaps.

## Staleness signals
- Deprecated or "no longer maintained" banners.
- The API, flag, or function is absent from current source.
- Date older than the library's most recent major release.
- Answer references removed tools or old language editions.
- Docs mention a version lower than the one in the lockfile.

## SEO-spam signals
- Listicle with no links to code, issues, or docs.
- Scraped or reworded official docs; same text mirrored across domains.
- No author, no date, generic stock phrasing.
- Affiliate links, heavy ads, "best X in 2026" titles.
- Contradicts official docs or source without evidence.
Trace such a claim to a primary source or drop it.

## Repository health (for library comparisons)
Check and record:
- Last commit and last release dates.
- Archived flag, deprecation notice in README.
- How open issues and PRs are handled: response time, stale backlog, maintainer replies.
- Number of active maintainers (bus factor); single-maintainer projects are a risk.
- Release cadence, semver discipline, open security advisories.
- Stars and downloads measure popularity, not quality. Do not use them as the deciding criterion.
