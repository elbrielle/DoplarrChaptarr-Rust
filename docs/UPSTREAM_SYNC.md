# Upstream sync status — activexray/doplarr_rs

**Synced @ `9533084`** (2026-08-28, sprint-3 branch): the merge below was
executed as planned. The three predicted conflicts were resolved by union
(both sides' backend variants and example sections kept), with one
architectural adaptation: upstream wires backends in `main.rs`, ours live in
`startup.rs` since the preflight split — the Sportarr arm was added there
and `main.rs` kept our thin form. Sportarr is carried per the sprint-3
decision (additive behind the shared trait; dropping it would fork us
permanently). Upstream's Radarr status feature and the new
`early_stop_message` trait default were taken as-is; Chaptarr keeps the
default (its `early_stop` never fires). Full workspace gates green
post-merge — every pre-merge Chaptarr test unchanged and passing — plus a
one-case live canary smoke (see `docs/chaptarr/canary/2026-08-28-0.9.936.md`).
`nix flake check` was not run on this machine (no nix); it remains on the
release checklist.

---

Checked 2026-08-27 against `upstream/main @ 9533084` (remote configured
fetch-only). Merge-base with our `main`: `aa2674c` ("chore: dep bump").

## Divergence

- **Upstream is 8 commits ahead** of the base: a new `sportarr_api` crate
  plus a Sportarr backend (PR #19), a Radarr "report current status of an
  already-requested movie" feature (PR #22, `fa67ca0`), a Sportarr
  path-encoding/add-response fix, and a formatting pass — 18 files,
  +1484/−219.
- **We are 31 commits ahead** of the base (the Chaptarr provider work:
  Sprint 1 rebaseline and Sprint 2 simplification/identity).

## Merge verdict: NOT clean — do not merge yet

A dry-run merge (`git merge-tree`) reports content conflicts in three files:

- `config.example.toml` (both sides added backend config sections)
- `doplarr/src/config.rs` (both sides extended `BackendConfig`)
- `doplarr/src/main.rs` (both sides extended backend wiring)

It would also auto-merge changes into `doplarr/src/providers/mod.rs` and
`doplarr/src/providers/radarr.rs` — files the Chaptarr sprints deliberately
do not touch — and vendor an entire generated `sportarr_api` crate that has
had no review here.

## What a merge would take

1. A dedicated branch after the sprint-2 review lands, so upstream churn is
   never entangled with a Chaptarr review diff.
2. Hand-resolution of the three config/wiring conflicts (mechanical: both
   sides add parallel backend variants; union-resolve and re-run
   `cargo fmt`).
3. A decision on whether to carry the Sportarr backend at all — it is dead
   weight for this deployment; if kept, it must stay behind its own config
   gate exactly like radarr/sonarr/seerr.
4. The Radarr status feature (`fa67ca0`) touches `providers/mod.rs` display
   plumbing shared with Chaptarr — re-run the full workspace suite and
   re-check the Chaptarr already-requested messages render unchanged.
5. Full gates before commit: `cargo test --workspace`, `cargo fmt --check`,
   `cargo clippy --workspace --all-targets`, plus `nix flake check` since
   `Cargo.lock` moves.

Until then, upstream remains fetch-only reference material.
