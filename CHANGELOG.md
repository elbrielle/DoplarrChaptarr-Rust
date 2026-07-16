# Changelog

All notable changes to DoplarrChaptarr Rust are documented here.

## 4.6.1-chaptarr.1 - Unreleased

Fixes the new-author request failure observed live on 2026-07-15. The old bot
accepted a Farseer collection row while Robin Hobb's catalog was still
refreshing, monitored and searched that bundle too early, and still reported
success when that `BookSearch` completed with zero results. It had not selected
a usable ebook edition, and the import tail later removed the partial monitor
state.

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
