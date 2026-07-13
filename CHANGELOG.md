# Changelog

All notable changes to DoplarrChaptarr Rust are documented here.

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
