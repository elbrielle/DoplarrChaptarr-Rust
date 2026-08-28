# Changelog

All notable changes to DoplarrChaptarr Rust are documented here.

## 4.6.1-chaptarr.1 - 2026-08-28

Fixes the new-author request failure observed live on 2026-07-15. The old bot
accepted a Farseer collection row while Robin Hobb's catalog was still
refreshing, monitored and searched that bundle too early, and still reported
success when that `BookSearch` completed with zero results. It had not selected
a usable ebook edition, and the import tail later removed the partial monitor
state.

The write path is beta, backed by the full disposable-instance mutation
canary of 2026-08-28 (`docs/chaptarr/canary/2026-08-28-0.9.936.md`): all
twelve checklist cases against a live `chaptarr/chaptarr:0.9.936` container.

### Changed - The native hook (Sprint 3)

- Startup metadata-profile resolution accepts the seeded General fallback:
  an explicitly configured name always wins (even `None` — the admin said
  so), then exactly one format-typed profile, then exactly one General
  profile excluding the `None` sentinel. Fresh installs resolve through the
  typed tier (migration 001 pre-seeds typed profiles — a canary correction
  to the source-read premise); legacy/imported databases whose profiles are
  all General now start instead of failing validation.
- The live canary showed work-id normalization is id-space-wide (a lookup's
  work id can vanish entirely at import, sidecars included), so
  server-asserted row links now stand beside the identity chain as
  authority: the `POST /book` 201 echo (the canonical row the add created
  or deduped to) resolves added works, and a lookup row's local-book
  association resolves already-local ones — format-checked, gated by a
  per-row already-requested check, and read-back verified by id. The chain
  still governs every row the server has not vouched for.
- A `POST /book` that fails after importing the catalog ("resolves to
  multiple local ... books", a live 500) is recovered in-request by
  resolving the just-imported rows through the identity chain; a 409 whose
  body is a bare error resource surfaces its message instead of an empty
  "identity is ambiguous ()".
- A headless canary driver (`cargo run -p doplarr --features canary --bin
  chaptarr_canary`) drives every RELEASE_CHECKLIST mutation case without
  Discord through the production startup path, with snapshot-based
  verification of every checklist bullet; `scripts/canary/` provisions the
  disposable instance. The `canary` feature keeps the driver out of the
  release binary set.
- `scripts/check-chaptarr-release.sh <tag>` triages a new Chaptarr release
  in minutes: route-inventory drift report plus the contract test against
  the fresh extract.
- Merged upstream doplarr_rs @ `9533084`: the Sportarr backend (`/request
  sport`) is carried, and Radarr gained upstream's already-requested status
  reporting. Chaptarr provider behavior is untouched (post-merge live smoke
  in the canary record).
- The Chaptarr HTTP helpers emit endpoint-only debug request lines, so a
  canary transcript carries every request with no key or search text.

### Changed - Simplification & identity (Sprint 2)

- The catalog-settle gate is rewritten around the server's actual
  scan-completion signal: `addOptions` absent on `GET /author/{id}` (the
  one-shot `AuthorScannedHandler` latch, spent on both the scan and skip
  paths) ANDed with quiet author-relevant commands, inside the same
  fail-closed deadline. The triple-fingerprint stability sampling is
  deleted, and a long-settled author passes the gate on the first look.
- Decision record `docs/chaptarr/decisions/0001-wanted-editions.md` records
  why `POST /book/{id}/editions/wanted` is not adopted (audiobook-only,
  add-incapable, gate-blind, swallowed re-pin, droppable non-manual search
  trigger); the release canary gains a one-time live probe of it.
- A contract-test layer fails CI when the narrow contract drifts: the 14
  depended-on routes are asserted against a vendored `openapi.json` paths
  extract (route inventory only - the spec's schemas are known-wrong), and
  every Chaptarr fixture is swept for serializer-trap violations (explicit
  nulls, `id: 0`, `grabbed`, `editions` on `/book` rows, mistyped
  `profileType`). `.github/ci/refresh-openapi-extract.sh` refreshes the
  extract in one command.
- Root folders resolve by their real discriminators: `folderType`
  (1=Audiobook, 2=Ebook) decides typed roots, nested `ebook`/`audiobook`
  settings-object presence decides Mixed roots, and `isEffectiveDefault*`
  breaks ties. The legacy boolean guard, name/path substring inference, and
  the lone-root fallback are deleted; unknown folder types and unconfigured
  formats fail closed.
- Local book rows are matched through a tiered cross-provider identity chain:
  exact `foreignBookId`, then the `goodreadsWorkId`/`goodreadsBookId`
  sidecars, then bare `asin`/`audibleASIN` equality, with the title tier only
  for selections that carry no identity at all. A row whose primary id
  normalized from `gr:` to `hc:` now short-circuits to the existing row
  instead of re-adding or failing.

### Changed - Chaptarr 0.9.936 rebaseline (Sprint 1: Truth & Correctness)

Every Chaptarr behavior contract is now derived from and cited against the
Chaptarr source at `v0.9.936` (`develop @ 423b1bb`) instead of live-probe
folklore; `docs/chaptarr/COMPATIBILITY.md` v2 carries the citations.

- The tested version line moved from `0.9.720` to `0.9.936` (fixtures, the
  `--check` preflight expectation, and the CI smoke assertion moved with it).
- All fixtures were regenerated to the real serializer's shapes: null
  properties and `id: 0` omitted (never string ids), no `grabbed`, no
  `editions` key on `/book` responses, free-text lookup rows as
  Goodreads-autocomplete projections (`gr:` ids, audiobook `mediaType`
  default, discriminator-free editions), proxied cover URLs with the absolute
  upstream only in `images[].remoteUrl`, per-format-only nested root-folder
  settings, and nullable `readingFormatId` on editions. A serializer-traps
  test module pins the key-absent tolerance.
- Editions are now discriminated by the structured `readingFormatId`
  (1=physical, 2=audio, 3=ebook; `isEbook: true` as the ebook-only fallback,
  everything else failing closed for writes). The free-text `format` field -
  verbatim provider text like "Kindle Edition" - is display and logging only.
  This fixes spurious "no usable edition" failures for correctly typed
  editions with provider-text labels.
- The placeholder completeness gate (`default-*` ids, required
  releaseDate/images) and its guarded `RefreshAuthor` repair path are deleted:
  0.9.936 mints no placeholders, and refresh has real deletion side effects.
  Sparse upstream metadata now flows through selection, monitoring, and
  verification like any other identity-matched row.
- `grabbed`-based in-flight detection is deleted: the flag is hardcoded false
  on the REST path and only ever set on SignalR broadcasts, so the check
  could never fire. In-flight detection is files present, an active exact
  `BookSearch`, or the in-process acknowledgement cache.
- Discord embed covers are read from `images[].remoteUrl`; on 0.9.936 both
  `images[].url` and `remoteCover` are relative proxied paths on lookup
  results, so without this no free-text result had an absolute cover.
- `POST /book` responses are branched on their three real shapes: a `202`
  `PendingBookRequestResource` stops with "Chaptarr queued this work upstream
  - try the request again in a few minutes", a `409`
  `ProviderAmbiguityResource` stops with a message naming the conflicting
  candidates, and a `201` remains an acknowledgement whose identity the bot
  re-resolves itself.

### Fixed

- New-author requests now wait for the catalog import to settle before any
  monitoring. The gate requires successful command polling, no author-relevant
  catalog command in flight, and stable book and target-edition state; an API
  error or deadline fails closed. The same wait runs after any bot-issued
  `RefreshAuthor`.
- Requests now select exactly one edition of the requested format (preferring
  English and the projected edition) via the full-book `PUT /book/{id}` with
  `anyEditionOk: false` and `manualAdd`, mirroring the Chaptarr UI. Edition truth
  is read exclusively from `GET /edition?bookId=...`; its authoritative
  `format` distinguishes Ebook, Physical, and Audiobook so a physical edition
  cannot be mistaken for an audiobook.
- This remains a single-work request flow. Results with clear multi-book title
  signals - such as a title ending in `bundle` or `trilogy`, an `omnibus`, a box
  set, a `complete ... series`, or an explicit numbered book collection/set -
  are rejected with an instruction to request each individual title. The bot
  does not expand one selection into many works.
- Duplicate import pockets (two local rows for one work, logged server-side as
  `SERVER-BUG-CANDIDATE`) are disambiguated by which row actually carries
  usable requested-format editions, and only that row is monitored.
  Already-requested checks now span every matching row so an available or
  in-flight twin stops a duplicate request. An exact active `BookSearch` and a
  recent valid acknowledgement retained by the same bot process are also
  deduplicated.
- The pre-search read-back now verifies the book is monitored AND its explicit
  requested-format flag is true, and that exactly one requested-format edition
  - the chosen one - is monitored before `BookSearch` is queued. A matching
  work with no usable edition fails with an actionable message.
- Retries now distinguish an available, grabbed, actively searched, or
  same-process recently acknowledged request from a partial monitor state. A
  prior edition/monitor write without that search evidence is repaired through
  the same verified sequence instead of being rejected as "already requested."
  This deduplication is deliberately bounded: after a bot restart, or after a
  search completes with no grab or file, an explicit retry may queue a fresh
  search instead of leaving a zero-result request permanently blocked.

This write-path change still requires the exact candidate image to pass the
disposable live mutation checklist before merge or release; fixtures and CI
cannot prove that private Chaptarr writes persist.

## 4.6.0-chaptarr.1 - Unreleased

This is a beta release candidate. Chaptarr's API is private and pre-1.0, and
the exact-work path for an already-local author still requires a disposable
live write/read-back test before public promotion. Use
`docs/chaptarr/RELEASE_CHECKLIST.md` for that gate.

### Added

- Native Chaptarr ebook and audiobook backends for `/request book` and
  `/request audiobook`.
- Read-only Chaptarr search with author-aware result labels, conservative junk
  filtering, and best-effort public book covers with optional, identified,
  cached, rate-limited Open Library enrichment.
- Per-format root-folder, quality-profile, and metadata-profile handling.
- Existing- and new-author request flows with format-scoped ebook/audiobook
  state, bounded metadata polling, monitor read-back verification, and one
  `BookSearch`.
- Legacy `CHAPTARR__*` environment-variable migration into the Rust TOML
  configuration.
- Sanitized Chaptarr `0.9.720.0` contract fixtures and compatibility guidance.
- A first-class `--check` preflight that validates every configured backend,
  reports compatible versions without connection details, and exits before
  Discord is contacted. Missing configuration and untested Chaptarr versions
  fail the gate instead of returning a false success.
- Exact-image CI smoke testing against deterministic Chaptarr fixtures plus a
  short-lived, checksummed workflow artifact for owner testing.

### Changed

- Chaptarr confirmation is read-only. No author or book is created merely to
  render a cover or confirmation screen.
- Chaptarr availability is checked before author-monitor settings are changed.
- Container and release packaging now targets the DoplarrChaptarr Rust image.
- Chaptarr API models/deserializers and pure selection policy now live in
  focused submodules, leaving provider I/O and mutations in the main provider.
- Main-image publication reuses the exact smoke-tested CI artifact and remains
  separately gated from ordinary pull-request builds.
- Discord interaction logs no longer format secret-bearing interaction objects.
- The combined project is distributed under GPL-3.0-only to match the linked
  generated Sonarr and Radarr client obligations while preserving upstream
  MIT/Apache notices.

### Compatibility

- Based on the Rust Doplarr `4.6.0` development line at upstream revision
  `aa2674c`.
- Chaptarr `0.9.720.0` is the initial tested API baseline.
- Root folders tolerate both explicit boolean format flags and the nested
  ebook/audiobook settings objects observed in Chaptarr `0.9.720.0`; nested
  objects are never interpreted as format flags.
