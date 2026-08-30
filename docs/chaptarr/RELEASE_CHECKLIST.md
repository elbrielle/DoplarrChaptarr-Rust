# Chaptarr beta release checklist

Use this checklist before publishing a DoplarrChaptarr build against a new
Chaptarr version. Chaptarr's API is private and pre-1.0, so unit fixtures prove
the client contract but cannot prove that writes still persist.

The Chaptarr provider is a single-work flow. This checklist does not approve
automatic series expansion. Results with clear multi-book title signals - such
as a title ending in `bundle` or `trilogy`, an `omnibus`, a box set, a
`complete ... series`, or an explicit numbered book collection/set - must be
rejected with an instruction to request individual titles.

## Preconditions

- Use a disposable Chaptarr instance or a disposable author/library root. Do
  not run mutation tests against a family production library.
- Record the exact DoplarrChaptarr commit, candidate-image digest, Chaptarr
  version and Chaptarr container digest. Test that exact candidate image.
- Back up Chaptarr's database and configuration and verify the restore path.
- Use synthetic or explicitly approved test books in both formats.
- Keep Open Library enrichment disabled if test search text must remain local.

## Automated gates

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- --deny warnings`
- `cargo build --release -p doplarr --locked`
- `nix flake check`
- Validate `docker compose config` without pasting its expanded secret values.
- Build the exact container with `nix build .#dockerImage`, load it, and run
  `.github/ci/smoke-image.sh IMAGE`; retain the resulting commit-specific image
  artifact and checksum.
- Run the candidate with `--check /config.toml` against the disposable
  Chaptarr instance and save its sanitized `discord: not_contacted` report.

## Read-only interaction proof

1. After preflight passes, start both `/request book` and `/request audiobook`
   backends.
2. Search each format and open the confirmation screen.
3. Abandon both interactions without pressing **Request**.
4. Confirm Chaptarr has no new author, book, monitor change or queued command.
5. Confirm covers render when available and a missing cover does not block the
   interaction.

## Mutation proof

Run each case once and inspect Chaptarr after every step:

1. Request an ebook for a new author.
2. Request an audiobook for a different new author.
3. Request an exact missing work under an already-local author.
4. Request the other format for a work whose first format is already local.
5. Repeat one request concurrently from two Discord users, then repeat it once
   immediately after a valid acknowledgement in the same bot process.
6. Select a result with a clear multi-book signal and press **Request**; verify
   the bot names the single-work limitation and makes no mutation.
7. In the disposable instance, prepare a partial state with the chosen edition
   and book monitored but no confirmed `BookSearch`, then repeat the request.
8. Inject or simulate a failed command poll during new-author settling and
   verify the request fails closed before edition, monitor, or search writes.
9. Complete a search with zero results and no file, or restart the bot after
   its recent acknowledgement is lost, then explicitly retry; verify at most one
   fresh search is queued by that new attempt.
10. Request a work whose row has sparse upstream metadata (no release date, no
    images); verify it resolves, monitors, verifies, and searches normally.
11. If reproducible, trigger a `202` pending add (upstream metadata not yet
    published) and a `409` ambiguous provider identity; verify the bot reports
    the retry-later / conflict message and made no further mutation.
12. Probe `POST /book/{id}/editions/wanted` once on a disposable audiobook
    row (decision record `docs/chaptarr/decisions/0001-wanted-editions.md`):
    confirm it rejects ebook rows, never changes the author's monitor gates,
    and that with the gate closed its `searchForNewBook` search is filtered
    out while an explicit `POST /command BookSearch` is not.
13. Sequential cross-format on one author, both directions: create a new
    author via `/request book` and later request an audiobook under it
    (the same work and a different work), then mirror the direction with
    another new author (`/request audiobook` first, `/request book`
    second). Restart the bot between the first and second request at least
    once. Chaptarr initializes author settings per format lazily
    (`COMPATIBILITY.md`, "Author settings are per-format and lazily
    initialized"), so the second request is what proves the
    missing-format initialization path.

For every case, verify:

- the selected title, author, `foreignBookId` when present and `mediaType` match;
- a lookup that returned a `gr:` work id still resolved the correct local row
  even if the server had normalized that row's primary id to `hc:` (the known
  identity-drift hazard from `docs/API_IDENTITY_AND_LIFECYCLE.md`);
- no sibling work or unrequested edition was monitored;
- the monitored edition's `readingFormatId` matches the requested format
  (2=audio, 3=ebook); a physical edition (`readingFormatId: 1`) is never
  selected for either format, whatever its provider `format` text says;
- the unrequested format's author gate did not change;
- top-level book monitoring AND the explicit requested-format monitor flag were
  both read back true;
- exactly one requested-format edition was monitored and matched the chosen ID;
- exactly one acknowledged `BookSearch` was queued after strict read-back;
- a concurrent request, exact active `BookSearch`, and immediate same-process
  retry after a valid acknowledgement did not queue another search;
- a retry repaired partial state instead of reporting it already requested;
- after restart or a completed zero-result/no-grab search, an explicit retry may
  queue one fresh search and does not queue more than one for that attempt;
- settle polling errors/timeouts produced a clear failure and no downstream
  mutation;
- no code path queued `RefreshAuthor` at any point;
- a sparse-metadata row was treated as ordinary requestable data, never as an
  "unresolved placeholder";
- (case 13) after the first request the author record carries only that
  format's settings — the sibling's profile ids and root keys are absent,
  per the lazy-initialization contract;
- (case 13) after the second request the author's requested-format quality
  profile id, metadata profile id, and root folder path read back equal to
  the resolved configuration — in particular the ebook root equals the
  configured **ebook** root, never the audiobook path (the
  progressive-fill root collapse);
- (case 13) the first format's settings, monitored edition, and files are
  unchanged, no duplicate author exists, and each request queued exactly
  one acknowledged search.

Assert case 13 against the author record (`GET /author/{id}`), not the
search outcome: on a zero-indexer instance an acknowledged `BookSearch` is
indistinguishable from one `ReleaseSearchService` silently empties for a
missing author quality profile.

## Sprint 1 write-path changes to prove live

The 0.9.936 rebaseline (Sprint 1: Truth & Correctness) changed these
write-path behaviors; the next canary must exercise each one explicitly:

- Edition selection, usability, and read-back verification discriminate by
  `readingFormatId` (with `isEbook: true` as the ebook-only fallback) instead
  of matching the free-text `format`.
- The add-body edition payloads now carry `readingFormatId` alongside the
  display `format`/`isEbook` fields.
- The completeness gate (releaseDate/images/`default-` id) is gone: sparse
  rows flow through selection, monitoring, and search.
- The bot never issues `RefreshAuthor`; the only commands it posts are
  `BookSearch`.
- `grabbed` no longer participates in already-requested detection; dedup rests
  on files, an exact active `BookSearch`, and the in-process ack cache.
- `POST /book` `202` and `409` responses stop the flow with user-facing
  messages instead of continuing into settle/poll.

**Proven live (2026-08-28, `docs/chaptarr/canary/2026-08-28-0.9.936.md`):**
`readingFormatId` selection and read-back (every case; a physical-only row
refused with zero mutations), sparse rows flowing through (case 10), no
`RefreshAuthor` anywhere, file/command/ack-based dedup (cases 5, 7, 9), and
202/409 stopping the flow (three live 202s; the live 409 was a bare-error
variant, recorded as finding #5).

## Sprint 2 write-path changes to prove live

The simplification-and-identity work (Sprint 2) changed these write-path
behaviors; the next canary must exercise each one explicitly:

- Local-row matching runs the tiered cross-provider identity chain
  (`foreignBookId` → `goodreadsWorkId` → `goodreadsBookId` → bare
  `asin`/`audibleASIN`): the existing identity-drift probe must confirm a
  `gr:` lookup short-circuits to an `hc:`-normalized local row with no
  re-add, and that the sidecar fields are actually present on imported rows.
- The settle gate is now the composite latch (`addOptions` absent on
  `GET /author/{id}` AND commands quiet) with no fingerprint sampling: a
  new-author add must be observed holding until `addOptions` disappears and
  proceeding promptly afterwards — on both the scan path and the
  no-folder-evidence skip path (no `RescanFolders` ever appearing) — and the
  write-before-settle failure must stay impossible at the deadline.
- Root resolution keys on `folderType` and nested settings presence (no
  name inference): the `--check` preflight against the live instance must
  resolve both formats' roots to the intended paths, and a format the
  instance has no configured root for must fail closed at startup rather
  than guess a folder.

**Proven live (2026-08-28, `docs/chaptarr/canary/2026-08-28-0.9.936.md`):**
the identity chain matched an exact-id pocket among drifted siblings (case
2) and the drift probe found the drift is id-space-wide — lookup ids can
vanish entirely at import, which is why server-asserted links (the `POST
/book` echo and the lookup's local-book association) now stand beside the
chain as authority (findings #1–#4). The composite settle latch held on
scan-path adds (passing on the first look because live imports run
synchronously inside the add — finding #6) and the failure injection left
no downstream write (case 8). Root resolution by `folderType` resolved both
formats' roots in the live `--check`.

Nothing in this sprint graduates the write path out of beta; that requires
this checklist's mutation proof against a disposable 0.9.936 instance —
now recorded in `docs/chaptarr/canary/2026-08-28-0.9.936.md`.

## Cross-format sequential settings to prove live

Open as of 2026-08-29. The 2026-08-28 canary's case 4 verified monitoring,
edition selection, and search acknowledgement on a cross-format second
request, but never read the author's per-format settings — and its
zero-indexer instance could not have surfaced the silent search-emptying
that a missing author quality profile causes. Source analysis
(`COMPATIBILITY.md`, "Author settings are per-format and lazily
initialized") has since established that Chaptarr persists only the
requested format's author settings per add, that a second-format request
heals settings only when it triggers an actual `POST /book` (the
progressive fill), and that the fill's single root parameter prefers the
audiobook path. The next canary must run mutation case 13 with its
author-record assertions before any release claims the sequential
cross-format path; the bot-side settings verification in safe-sequence
step 9 is not yet implemented and must land first.

## Promotion record

Save a sanitized release note with the versions, digest, cases run, results and
rollback artifact. Remove API keys, Discord IDs/tokens, search titles, local
paths and internal hostnames. Only after this record is complete should a beta
be published; only a later explicitly stable tag may move the `latest` image.
