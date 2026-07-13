# Chaptarr compatibility contract

This document defines the deliberately small part of Chaptarr's API that
DoplarrChaptarr depends on. Chaptarr is still pre-1.0 and does not publish a
machine-readable API contract, so the Rust client is handwritten, tolerant of
additive fields, and tested against sanitized response fixtures.

The compatibility baseline is Chaptarr `0.9.720.0`. The published container
tag is [`robertlordhood/chaptarr:0.9.720`][chaptarr-tags]. The fixture shapes
were derived from read-only inspection of that release on 2026-07-12, the
previous DoplarrChaptarr implementation, and its recorded live-test findings.
They are examples, not a promise that Chaptarr will never add or omit fields.

The base URL is `<CHAPTARR_URL>/api/v1`. Send the API key in `X-Api-Key`; never
put it in a URL, log line, fixture, or Discord response.

## Supported endpoints

| Method | Endpoint | Purpose | Required response data |
| --- | --- | --- | --- |
| `GET` | `/system/status` | Startup compatibility check | `appName: "Chaptarr"`, non-empty `version` |
| `GET` | `/book/lookup?term=...` | Read-only search before confirmation | title, author identity, foreign work identity, images/editions when present |
| `GET` | `/author` | Resolve an already-local author | `id`, `foreignAuthorId`, `authorName` |
| `GET` | `/author/{id}` | Read and verify the requested-format author gate | `id`, `ebookMonitorFuture`, `audiobookMonitorFuture` |
| `GET` | `/book?authorId=...` | Poll and rank local format rows | fields listed under "Book rows" |
| `GET` | `/book/{id}` | Verify status and monitor writes | fields listed under "Book rows" |
| `GET` | `/qualityprofile` | Resolve per-format quality profiles | `id`, `name`, `profileType` |
| `GET` | `/metadataprofile` | Resolve per-format metadata profiles | `id`, `name`, `profileType` |
| `GET` | `/rootfolder` | Resolve accessible roots | `id`, `path`; `accessible` when present |
| `POST` | `/book` | Add a new author/catalog or an exact work under an existing author | created book row with `authorId`, or enough identity to re-resolve it |
| `PUT` | `/author/{id}` | Enable one format's author-level monitor gate | response is not trusted; verify with `GET` |
| `PUT` | `/book/monitor` | Monitor one selected book row | HTTP success only; verify with `GET /book/{id}` |
| `POST` | `/command` | Queue one `BookSearch`, or a narrowly gated `RefreshAuthor` | command acknowledgement; the body is otherwise opaque |

Unknown response fields must be ignored. Fields used only for ranking or
covers must be optional. A missing identity, format discriminator, or local
row ID is not optional: stop before a write rather than guessing.

## Shape and field rules

### Lookup results

`GET /book/lookup` returns an array. Chaptarr `0.9.720.0` can include:

- `id`, sometimes numeric and sometimes string-like in older or transitional
  responses. Only a positive integer means a local row; `0`, `"0"`, null, and
  malformed strings mean "not local."
- `title`, `titleSlug`, `overview`, `releaseDate`, and `foreignBookId`.
- `author.id`, `author.authorName`, and `author.foreignAuthorId`.
- `images`, `remoteCover`, and edition-level `images`.
- `localEbookBooks` and `localAudiobookBooks`. Positive IDs in the array for
  the requested format are a better local shortcut than the top-level `id`.
- `editions`, which may carry `foreignEditionId`, `isEbook`, `isbn13`, `asin`,
  language, and additional cover images.

Lookup order is not stable and is not a ranking guarantee. Never use a local
audiobook ID to satisfy an ebook request, or the reverse.

### Profile and root discriminators

Chaptarr uses two different representations for `profileType`:

- Quality profiles: `"ebook"` or `"audiobook"`.
- Metadata profiles: `2` for ebook, `1` for audiobook, and `0` for none.

The new-author request must carry all four per-format profile IDs. The legacy
singular `qualityProfileId` and `metadataProfileId` fields were silently
ignored in live testing. Resolve configured profile names exactly at startup;
if a configured name is missing or ambiguous, fail configuration validation.

Roots are selected by exact configured path or name. When no root is configured,
the client uses Chaptarr's format/default flags, then conservative `ebook` or
`audiobook` path/name inference only if those flags are absent. Ambiguity stops
startup. A root explicitly marked `accessible: false` is never selectable. Do
not expose a local root path in a Discord message or public issue report.

Chaptarr `0.9.720.0` may return the root-folder keys `ebook` and `audiobook` as
nested settings objects on every root, rather than boolean format flags. Those
objects are accepted for compatibility but never treated as discriminators;
only an explicit boolean `true` may select a format. Exact configured paths are
therefore preferred, with conservative path/name inference as the fallback.

### Book rows

Chaptarr `0.9.720.0` exposes row-oriented state:

- `id` and `authorId` identify the local row and parent author.
- `mediaType` is `"ebook"` or `"audiobook"`.
- `monitored`, `hasFiles`, `grabbed`, and `statistics` describe that row.
- Older shapes may additionally expose `ebookMonitored`,
  `audiobookMonitored`, `ebookStatistics`, and `audiobookStatistics`.
- `foreignEditionId`, `releaseDate`, and `images` distinguish resolved rows
  from placeholders.
- `ratings.popularity`, `ratings.votes`, and release date are optional ranking
  tie-breakers only.

Always bind a row-oriented flag to its `mediaType`. An ebook row's
`monitored`, `hasFiles`, or `statistics` must never satisfy an audiobook
request.

For the requested format, status is evaluated in this order:

1. **Available:** matching row has `hasFiles: true`, or its matching statistics
   report a positive `bookFileCount`.
2. **Already requested:** matching row has `grabbed: true` or is monitored.
3. **Unmonitored:** neither condition is true.

Where per-format legacy fields exist, use only the pair belonging to the
requested format. Prefer the row-oriented `0.9.720.0` fields when `mediaType`
is present.

### Resolved rows and placeholders

A row is safe to monitor only when all three are true:

- `releaseDate` is present;
- `images` is non-empty;
- `foreignEditionId` is present and does not start with `default-`.

A row missing any of those signals is a placeholder. Chaptarr has historically
returned success while dropping monitor changes against placeholders. Poll for
a complete, title-matching row with a hard attempt and time limit. If it never
resolves, report a metadata-pending error; do not search a sibling work.

`RefreshAuthor` may be attempted once, after confirmation, only when the exact
target remains a placeholder. It is not a general retry: refresh can reapply
metadata-profile filters and remove editions.

## Search and selection invariants

Search should be useful without becoming inventive:

1. Normalize whitespace, case, and punctuation for comparison while retaining
   the original title for display.
2. Display the author beside the title so common titles are distinguishable.
3. Drop obvious non-work results such as study guides, SparkNotes/CliffsNotes,
   summaries and analysis, unofficial companions, lesson plans, and
   conversation starters. Use conservative multi-word markers; do not reject
   a legitimate title merely because it contains `guide` or `summary`.
4. Prefer exact normalized title matches, then narrowly allow subtitle variants
   separated by `:`, `-`, `—`, or parentheses. A plain shared prefix is not a
   match. Never cross authors after the user has selected one.
5. Within that title tier, prefer an explicit requested `mediaType`, a resolved
   row, and a title whose length is closest to the selected lookup title.
   Popularity, votes, and release date are final tie-breakers.
6. If no row matches the selected title and format, stop. Falling back to a
   popular sibling title requests the wrong book.

Provider IDs are hints, not universal identity. Lookup may return a Goodreads
author ID while the local author was normalized to Hardcover. Resolve authors
first by exact `foreignAuthorId`; if that fails, allow a normalized author-name
fallback only when exactly one local author matches.

## Cover selection

Cover rendering is strictly read-only. The old Clojure fork created an author
while building the confirmation embed because the post-add row often had a
better cover. That left catalog state behind when a user abandoned the dialog.
The Rust implementation must never `POST`, `PUT`, or queue a command merely to
obtain a cover.

Use the first safe option available:

1. A fully qualified HTTPS `cover` image on the lookup result.
2. A fully qualified HTTPS edition image, then a fully qualified HTTPS
   `remoteCover`.
3. An Open Library ISBN cover URL when the selected edition carries a valid
   ISBN-13: `https://covers.openlibrary.org/b/isbn/<isbn>-L.jpg?default=false`.
4. A cover from one best-effort Open Library Search API call per Chaptarr
   search. Query by the selected title and author, request only the fields the
   matcher needs, and accept `cover_i` only from a normalized title-and-author
   match. Construct
   `https://covers.openlibrary.org/b/id/<cover_i>-L.jpg?default=false`.
5. No cover. A cover failure never blocks a request.

Relative Chaptarr cover URLs are not exposed because they often contain an
internal hostname and this release has no separately configured public base
URL. Open Library enrichment defaults to enabled but may be disabled per
backend with `openlibrary_covers = false`; when enabled, the search text leaves
the local network. The client identifies itself, caches results, and serializes
requests to remain at or below one request per second.

Open Library documents both the Search API's `cover_i` field and the Covers
API's Cover ID/ISBN URL formats. `default=false` returns 404 instead of a blank
placeholder. A local ISBN avoids an additional metadata request; when search
enrichment is needed, its Cover ID URL is used. Construct a display URL; do not
crawl or probe the Covers API during search. The Search API call needs an
explicit short timeout and must
degrade to the next option on timeout, non-2xx status, malformed JSON, no exact
match, or no `cover_i`. See the [Open Library Search API][openlibrary-search]
and [Open Library Covers API][openlibrary-covers]. ASIN-derived Amazon image
URLs from the legacy fork are intentionally not part of this compatibility
contract because there is no stable public API guarantee for them.

## Safe request sequence

The sequence below is an invariant. Keeping confirmation read-only and
verifying every silent-write-prone step is more important than minimizing GETs.

1. At startup, fetch status, profiles, and roots. Validate version and all
   configured names/paths before accepting a command.
2. Search with `GET /book/lookup`. Filter and rank the result. One bounded,
   best-effort Open Library metadata lookup may enrich covers for that search;
   fetch no cover bytes and perform no write.
3. Let the user select a result, format-specific options, and explicit Request
   confirmation. Disable the Request button immediately after the click.
4. Re-resolve the selected lookup identity. Read local author/book state and
   short-circuit available or already-requested rows before changing an author.
5. If the author is new, `POST /book` with both roots, all four profile IDs,
   every book-level monitor flag false, only the requested format's author-level
   `*MonitorFuture` gate true, and search-on-add false. If the author exists but
   the exact work does not, post the selected work with that local `authorId`.
   A post response is only an acknowledgement and never implies that a usable
   or correctly identified row already exists.
6. Poll `GET /book?authorId=...` with a bounded deadline. Select only the exact
   title/author/requested-format row; when both sides expose `foreignBookId`, it
   must match. Require a resolved row and repeat the identity check immediately
   before monitoring and after the monitor read-back.
7. Read the author. If the requested format's `*MonitorFuture` gate is false,
   set only that gate (plus the required top-level `monitored` field), then
   re-read the author to verify it.
8. `PUT /book/monitor` with one ID: `{"bookIds":[id],"monitored":true}`.
9. Re-read `/book/{id}` and verify the requested-format monitor state. If it did
   not persist, stop and do not queue a search.
10. `POST /command` once with `{"name":"BookSearch","bookIds":[id]}`.
11. Treat retries as idempotent: re-read status first, never create duplicate
    authors, and never queue multiple searches from one Discord interaction.

The existing-author exact-work POST is based on the current Moonrock Helper
integration and sanitized fixture shapes, not a write test against Chaptarr
`0.9.720.0` performed for this release. The request uses an explicit allowlist
and keeps all book and edition monitor flags false. It remains beta until a
disposable-library write/read-back test proves that shape on a published
Chaptarr build.

Network calls need explicit connect and total-request timeouts. Polling needs a
deadline and cancellation when the Discord interaction is no longer usable.
The async runtime must not be blocked by synchronous waits.

### New-author payload requirements

The payload below shows the required semantics; values are illustrative:

```json
{
  "title": "Selected title",
  "foreignBookId": "provider:work-id",
  "monitored": false,
  "ebookMonitored": false,
  "audiobookMonitored": false,
  "rootFolderPath": "/selected-format-root",
  "ebookQualityProfileId": 11,
  "audiobookQualityProfileId": 12,
  "ebookMetadataProfileId": 21,
  "audiobookMetadataProfileId": 22,
  "author": {
    "authorName": "Selected author",
    "foreignAuthorId": "provider:author-id",
    "ebookQualityProfileId": 11,
    "audiobookQualityProfileId": 12,
    "ebookMetadataProfileId": 21,
    "audiobookMetadataProfileId": 22,
    "rootFolderPath": "/selected-format-root",
    "ebookRootFolderPath": "/ebook-root",
    "audiobookRootFolderPath": "/audiobook-root",
    "ebookMonitorFuture": true,
    "audiobookMonitorFuture": false,
    "monitored": true,
    "monitorNewItems": "none",
    "addOptions": {
      "monitor": "none",
      "searchForMissingBooks": false
    }
  },
  "addOptions": {
    "searchForNewBook": false
  }
}
```

After the row resolves, the requested format is verified at the author level
and enabled on the one selected book row. Starting every book row unmonitored
prevents one format request from accidentally monitoring every edition or the
other format.

## Fixture provenance

Fixtures live in `doplarr/tests/fixtures/chaptarr/`. All titles, names, IDs,
paths, dates, and URLs are synthetic. No Moonrock user, library, API key, or
private hostname appears in them.

| Fixture | Evidence represented |
| --- | --- |
| `system_status.json` | Live `0.9.720.0` status discriminator |
| `lookup.json` | Live lookup fields, local ebook/audiobook arrays, relative and absolute cover shapes; junk row added synthetically to preserve the legacy filter regression |
| `openlibrary_search.json` | Official Search API `cover_i` shape with exact and non-matching title/author rows for cover-enrichment tests |
| `author.json` | Fields used by live author resolution and the two monitor gates |
| `book_available.json` | `0.9.720.0` row-oriented format/file/statistics shape |
| `book_processing.json` | Row-oriented monitored/grabbed state with no files |
| `book_unmonitored.json` | Resolved row eligible for the monitor sequence |
| `book_placeholder.json` | Legacy live-test placeholder invariant (`default-*`, no date, no images) |
| `quality_profiles.json` | Observed string `profileType` discriminators |
| `metadata_profiles.json` | Observed integer `profileType` discriminators |
| `root_folders.json` | Required root identity/path/accessibility fields |
| `post_book_response.json` | Legacy-observed created-book response carrying top-level `authorId`; exact values are synthetic |
| `put_monitor_response.json` | Legacy-observed 202 status snippet; text is illustrative and must not be parsed for verification |
| `command_response.json` | Servarr-style queued-command acknowledgement used by Chaptarr; only acknowledgement is relevant and all values are illustrative |

The three mutation-response fixtures are intentionally non-authoritative. The
implementation must deserialize them tolerantly, then establish truth through
the read-back steps above.

## Drift policy

Chaptarr is pre-1.0, so a patch-looking release can still change an API shape.
Compatibility is maintained as follows:

- Parse only fields this contract uses and tolerate unknown fields.
- Treat optional ranking, status, and cover fields as nullable and type-drifting
  where older responses have demonstrated number/string variation.
- Require `appName: "Chaptarr"` and a non-empty live version at startup.
  `0.9.720.x` is the tested baseline; a different non-empty version receives a
  clear untested-version warning.
- Run the exact candidate image with `--check /config.toml` before Discord
  startup. The command must exercise status, root, quality-profile, and
  metadata-profile parsing, emit only a sanitized summary, and exit without
  constructing a Discord client. A version outside the tested line must produce
  an explicit `unsupported` report and nonzero exit even though normal startup
  retains warning-only compatibility behavior.
- If required identity or format fields are absent, disable Chaptarr writes for
  that interaction and return a useful compatibility error. Do not guess.
- Before claiming support for a new release, run read-only lookup/detail,
  profile, root, and status probes; sanitize any new shapes; update these
  fixtures; and run contract tests.
- A write-path change also requires a disposable or explicitly approved live
  request test. Read-only evidence cannot prove a mutation still persists.
- Never regenerate a broad Readarr client from an unrelated OpenAPI document.
  Chaptarr is Readarr-like, but its format-specific fields and monitor behavior
  are the reason this narrow contract exists.

Rust Doplarr's own developer guide explicitly supports adding a backend through
the `MediaBackend` and `MediaItem` traits. Keeping Chaptarr behind that provider
boundary makes this compatibility layer replaceable without forking Discord
interaction machinery; see [Doplarr developer documentation][doplarr-dev].

[chaptarr-tags]: https://hub.docker.com/r/robertlordhood/chaptarr/tags
[openlibrary-search]: https://openlibrary.org/dev/docs/api/search
[openlibrary-covers]: https://openlibrary.org/dev/docs/api/covers
[doplarr-dev]: https://github.com/activexray/doplarr_rs/blob/main/README_DEVELOPER.md
