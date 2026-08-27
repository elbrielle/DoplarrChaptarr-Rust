# Sprint 3 Packet — The Native Hook

**Product:** DoplarrChaptarr-Rust · **Owner:** Elisha Lucero · **Dates:** 2026-09-10 → 2026-09-16
**Sprint goal:** The verified request pipeline is proven live against a real, disposable Chaptarr
0.9.936 instance; the fork is synced with upstream doplarr_rs; and the release is staged so that
Elisha's sign-off — not this sprint — is the only thing between the write path and beta
graduation.

This packet is self-contained. Its appendix (§7) carries line-verified operational facts about
running Chaptarr in Docker as a canary target, gathered per the verify-before-send-off process.
That verification also found a launch-blocking bug in our own resolver (§3, story 2.7) — read it
first.

---

## 1. Working agreements (unchanged; read first)

**Harness model.** Fable designs, manages, audits, and gates everything complex; Opus subagents
do rudimentary work. Nothing merges without Fable-level review of the actual diff.

**Authorship.** Everything authored as Elisha Lucero. No AI attribution anywhere.

**Engineering bar.** Root cause + source citation in commit messages; revert-failing tests;
`cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets` clean
(PATH needs `/opt/homebrew/opt/rustup/bin`); radarr/sonarr/seerr behavior untouched except where
story 6.2 merges upstream's own changes to them; comments only for real constraints; fixtures
and docs travel with behavior changes; no band-aids.

**Git.** Branch `sprint-3-native-hook` off `main`. Local commits only; no push, no tag, no
release, no PR until Elisha reviews.

**Hard limits for this sprint (violating any of these fails the sprint):**
- Nothing is tagged, published, or announced. Beta graduation is claimed by Elisha's promotion
  record, never by this sprint's output.
- No GitHub issues are posted upstream — story 6.3 produces local drafts only.
- The canary touches only the disposable instance defined in §4; never any URL from a real
  config. Search terms sent to the live instance leave the local network (Goodreads +
  api2.chaptarr.com) — use synthetic or well-known public titles only, never personal library
  data. Mount only empty throwaway directories as roots.

---

## 2. Scope

| # | Story | Pts | Scope |
|---|-------|-----|-------|
| 2.7 | Metadata-profile resolution: accept the seeded General fallback | 2 | CORE |
| 5.2a | Headless canary driver | 3 | CORE |
| 5.2b | Disposable instance + full canary run + evidence record | 3 | CORE |
| 5.3 | Release-watch script | 1 | CORE |
| 6.2 | Upstream doplarr_rs merge (carry Sportarr) | 3 | CORE |
| 6.1 | Release staging (notes, docs, version, promotion record draft) | 2 | CORE |
| 6.3 | Upstream-issue drafts (local only) | 1 | STRETCH |

Committed 14 pts, stretch 1. **Cut order if squeezed:** 6.3 → 6.1 → 6.2. The canary lane
(2.7 → 5.2a → 5.2b) never slips — it is the sprint.

**Sequencing:** 2.7 first (the canary cannot even start the bot without it). Then 5.2a and 5.3
in parallel. 5.2b after both. 6.2 only after the canary evidence is recorded (the canary must
test the tree we audited, not a fresh merge). 6.1 last, incorporating 6.2's outcome. After 6.2,
re-run the automated gates plus a one-case canary smoke (new-author ebook) to prove the merge
did not disturb the provider.

**Out of scope:** the `MediaBackend` trait change for success-embed enrichment (deferred to the
Later lane — a release sprint is the wrong place for a cross-provider signature change; see
Sprint 2's cut rationale); adopting `/editions/wanted` (decision 0001 stands); moving any
`latest` image or tag; the Discord read-only interaction proof (Elisha's, §6).

---

## 3. Story details

### 2.7 Metadata-profile resolution accepts the seeded General fallback (2 pts)

**Defect (launch-blocking, found by operational verification):** a fresh Chaptarr install seeds
quality profiles for both formats (`eBook`/type-Ebook, `Spoken`/type-Audiobook) but its two
seeded metadata profiles — `Standard` and `None` — are both **type General (0)**
(§7.4). Our `resolve_profile` requires metadata `profileType == 2` (ebook) or `== 1`
(audiobook), so against a default instance the bot fails configuration validation at startup
and never connects. The server itself accepts a General metadata profile anywhere a typed one
is wanted (§7.4).

**Fix (decided):** metadata-profile resolution becomes: explicitly configured name always wins
(ambiguity fails closed, unchanged) → a format-typed profile (1/2) when exactly one matches →
otherwise fall back to General (0) profiles **excluding any named `None`** (the seeded `None`
is a filter-everything sentinel, `MinPopularity 1e10` — selecting it would break imports,
§7.4); exactly one surviving General profile is used, more than one fails closed with the
existing actionable error. Quality-profile resolution is unchanged (fresh installs seed exactly
one per type).

**Acceptance criteria**
- A fixture modeling the fresh-install seed (`Standard` + `None`, both `profileType: 0`)
  resolves to `Standard` for both formats.
- Typed profiles still win over General when present; two non-`None` General profiles without
  explicit config fail closed; explicit config naming `None` is honored (the admin said so).
- COMPATIBILITY.md profile section updated with the seeding facts and citations.

### 5.2a Headless canary driver (3 pts)

The RELEASE_CHECKLIST mutation proof is drivable without Discord: every case is a
`MediaBackend::search` / `additional_details` / `request` sequence. Build a dev-only binary
(`cargo run -p doplarr --bin chaptarr_canary`, excluded from the release/docker artifacts)
that:

- Reads `CHAPTARR_URL`, `CHAPTARR_API_KEY`, and per-case arguments; constructs the two backends
  (ebook + audiobook) exactly as `connect()` does in production, including `--check`-equivalent
  startup resolution.
- Executes the checklist's mutation cases 1–12 as named subcommands or a scripted suite,
  mapping the Discord-shaped steps to direct calls: "two Discord users" = two concurrent
  `request()` futures with distinct requester ids; "press Request on a bundle" = `request()`
  with a multi-book title; the partial-state case prepares state via direct API writes to the
  disposable instance before invoking `request()`.
- After each case, performs the checklist's verification reads (author gates, book flags,
  editions with `readingFormatId`, command history) and emits a per-case PASS/FAIL transcript
  with every request line and a sanitized summary (no API keys, no local paths).
- Asserts `BookSearch` **acknowledgement**, never results — the disposable instance has zero
  indexers, and a zero-indexer search is a verified server-side no-op (§7.7).
- Treats a `202 PendingBookRequestResource` on add as a legitimate recorded outcome (retry
  later), not a failure (§7.3).

**Acceptance criteria:** driver builds and runs against a mock (unit-level smoke); it is not
part of the release binary set; transcripts are reproducible and sanitized by construction.

### 5.2b Disposable instance + full canary run (3 pts)

Provision and run entirely locally under `scratch/canary/` (gitignored) with a committed
template in `scripts/canary/`:

1. `docker-compose.yml`: image `chaptarr/chaptarr:0.9.936` (exact version tags are published,
   §7.1), port `8789:8789`, throwaway `./config:/config`, and two **empty** throwaway root
   mounts `/audiobooks` and `/ebooks`; preseed the API key via `Chaptarr__Auth__ApiKey` so the
   run is fully headless (§7.2). Record the image digest.
2. Wait for readiness: `/ping` transitions 503 → `200 {"status":"OK"}` (§7.6), **then** poll
   `GET /api/v1/qualityprofile` until it returns the 2 seeded profiles — seeding runs after
   `/ping` goes green (§7.4/§7.6).
3. Provision roots via `POST /api/v1/rootfolder` with the minimal typed bodies from §7.5
   (paths must exist and be writable in-container; the add queues a local `RescanFolders` on
   the empty dir — harmless).
4. Run the candidate `--check` against the instance and save the sanitized report; then run the
   full 5.2a suite for both formats.
5. Write the evidence record to `docs/chaptarr/canary/2026-09-<dd>-0.9.936.md`: DoplarrChaptarr
   commit, Chaptarr version + container digest, cases run, per-case results, anomalies, and
   the identity-drift observation (did imported rows carry `goodreadsWorkId` sidecars, did a
   `gr:` lookup short-circuit to an `hc:` row). Sanitized per the checklist.
6. Tear down completely; nothing from `scratch/canary/` is committed.

If the pinned image tag is unavailable or the live version differs from 0.9.936: record it, run
the read-only probes and contract tests first, and proceed only if they pass — per the drift
policy. A live finding that contradicts a COMPATIBILITY claim is itself a sprint deliverable:
record it, fix or file it, never paper over it.

**Acceptance criteria:** every mutation case executed (or explicitly recorded blocked, with the
exact reason); all checklist verification bullets confirmed in the record; the two "prove live"
sections of RELEASE_CHECKLIST marked with results; teardown verified.

### 5.3 Release-watch script (1 pt)

`scripts/check-chaptarr-release.sh <git-ref>`: clone Chaptarr at the ref (shallow), extract its
openapi paths to a temp file, diff against our vendored extract, run the route-inventory
contract test against the fresh extract, and print a drift report (routes gained/lost, our 14
depended-on routes intact or not). Non-zero exit on a lost depended-on route. This is how a new
Chaptarr release gets triaged in minutes.

### 6.2 Upstream doplarr_rs merge (3 pts)

Execute the merge documented in `docs/UPSTREAM_SYNC.md` on the sprint branch (or a sub-branch),
**after** the canary evidence is recorded.

**Decision (made, not open):** carry Sportarr. Staying mergeable with upstream is the point of
the provider boundary; the Sportarr crate and backend are additive behind the same trait, and
dropping them would fork us permanently. Resolve the three documented conflict files preserving
both sides' backends; take upstream's radarr status feature as-is.

**Acceptance criteria:** merge commit authored as Elisha; full workspace gates green including
the new sportarr crate; every Chaptarr provider test untouched and green (behavior-neutral for
our provider); config.example.toml documents all backends; post-merge one-case canary smoke
passes; UPSTREAM_SYNC.md updated to "synced @ <sha>".

### 6.1 Release staging (2 pts)

- CHANGELOG: consolidate the unreleased section into dated release notes; version per the rule:
  if 6.2 adopted an upstream version bump, follow it and keep the `-chaptarr.N` suffix;
  otherwise release as `4.6.1-chaptarr.1`.
- README + MIGRATING refreshed to current truth (Chaptarr provider status, canary-backed beta
  statement, config keys).
- Draft the promotion record from the canary evidence, leaving Elisha's sign-off fields
  explicitly empty (see §6).
- Stage everything as commits on the branch. **Do not tag, publish, or claim graduation.**

### 6.3 STRETCH — upstream-issue drafts (1 pt)

Write `docs/chaptarr/upstream-issues/*.md` drafts (local only, Elisha posts or discards):
the openapi spec mistyping `CommandResource.body` and parameter optionality; the dead
`Book.Monitored` legacy column and never-set `IsFallbackEdition` flag; optionally the
root-folder profile-id validators that are injected but never attached (§7.5). Each draft:
observed behavior, source citations, why it matters to API clients, suggested fix. Courteous,
no entitlement — we're a downstream client saying thanks.

---

## 4. Definition of done (sprint level)

- [ ] All core stories merged to `sprint-3-native-hook`, each meeting §1's bar.
- [ ] Canary evidence record committed with every checklist verification bullet answered.
- [ ] Full gates green at branch tip (post-merge), including the merged upstream code.
- [ ] Release notes + promotion record staged with Elisha's sign-off fields empty.
- [ ] Closing summary for Elisha: canary results (especially any live surprise), merge outcome,
      what remains for her sign-off, and anything that should shape the Later lane.

## 5. Elisha-only items (the sprint stages these; it never performs them)

1. The RELEASE_CHECKLIST **read-only interaction proof** — real Discord bot, real guild,
   abandon-without-Request cases, cover rendering. Needs her token and eyes.
2. Promotion record sign-off → beta graduation statement.
3. `git push`, release tag, and any published artifact.
4. Posting any upstream issue from the 6.3 drafts.

---

## 6. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Live 0.9.936 behavior contradicts a source-derived claim | Canary case fails | That is the canary working: record precisely, fix at the root or re-scope; never adjust the test to pass |
| api2.chaptarr.com pending-materialization (202) on test titles | Cases stall | Use well-known public-domain/major titles; the driver records 202 as a legitimate outcome and retries once after a delay |
| Docker unavailable / image tag missing | 5.2b blocked | Record the blocker; run every non-live story; the canary then becomes the single carry-over, clearly flagged |
| Upstream merge destabilizes shared code | Release risk | Merge only after canary evidence is recorded; full gates + Chaptarr tests unchanged + one-case re-smoke |
| Fresh-install profile bug has siblings (other seeded-state assumptions) | Startup failures for real users | 2.7's fixture models the true seeded state; `--check` against the live instance in 5.2b validates the whole startup path |

---

## 7. Appendix — verified operational facts (Chaptarr v0.9.936, `develop @ 423b1bb`)

### 7.1 Docker
- Published to Docker Hub as `chaptarr/chaptarr` and `robertlordhood/chaptarr`
  (`.github/workflows/docker-publish.yml:20-21`); exact version tags from git tags with `v`
  stripped — `chaptarr/chaptarr:0.9.936` exists (`:91-94`); `main`→`:main`+`:latest`,
  `develop`→`:develop`; no rolling minor tags (deliberate, `:89-91`).
- Internal port **8789** (`Dockerfile.cross:117`, `ConfigFileProvider.cs:185`); compose shape:
  `./config:/config`, media mounts, `PUID/PGID/TZ` env, `8789:8789` (repo `docker-compose.yml`).
  No image healthcheck — use `/ping`.
- Entrypoint defaults PUID=99/PGID=100 when unset and hard-fails if `/config` is not writable
  (`docker-entrypoint.sh:5-6,156-162`).

### 7.2 Auth / API key
- API key auto-generated (de-hyphenated GUID) into `/config/config.xml` on first boot
  (`ConfigFileProvider.cs:193-207,527-530`); **preseedable via env `Chaptarr__Auth__ApiKey`**
  (`AuthOptions.cs:5`, bound at `Bootstrap.cs:201-205,280-284`). It cannot be set through
  `PUT /config/host` (`ConfigFileProvider.cs:145-150`).
- **No setup wizard gates the API.** Every `/api/v1` controller authenticates via the `API`
  scheme (`X-Api-Key` header, `apikey` query, or `Bearer`) regardless of UI auth state
  (`Startup.cs:199-203`, `ApiKeyAuthenticationHandler.cs:42-107`); default UI auth method is
  `None` (`ConfigFileProvider.cs:223`). Other useful env keys: `Chaptarr__Server__Port`,
  `Chaptarr__Server__UrlBase`, `Chaptarr__Server__BindAddress`.

### 7.3 Metadata dependency
- Default metadata server: **`https://api2.chaptarr.com`** (hosted, no token; config key
  `MetadataServerUrl`, `ConfigService.cs:404-408`); requests carry only a User-Agent
  (`BookInfoProxy.cs:120,309`). Free-text lookup additionally hits
  `goodreads.com/book/auto_complete` directly (`GoodreadsSearchProxy.cs:29-33`); its failure
  degrades to an empty result, not an error.
- A fresh install with internet works for lookup and add with zero configuration. `POST /book`
  may answer **202 pending** while the hosted service materializes an author — legitimate,
  retry later (`BookController.cs:1262-1307`). Hardcover token not required (disabled by
  default; gates only series lookup).

### 7.4 First-boot seeding
- Quality profiles seeded once, on `ApplicationStartedEvent`: `eBook` (type Ebook/2, cutoff
  MOBI) and `Spoken` (type Audiobook/1, cutoff M4B) (`QualityProfileService.cs:118-163`).
- Metadata profiles seeded: `Standard` and `None`, **both type General (0)** —
  `MetadataProfile.ProfileType` defaults to General and the seeder never sets it
  (`MetadataProfileService.cs:647-701`, `MetadataProfile.cs:6-33`). `None` is a
  filter-everything sentinel (`MinPopularity 1e10`). The server accepts General wherever a
  typed profile is wanted (`MetadataProfileController.cs:138`).
- **Timing:** seeding fires after `/ping` already returns 200 (`AppLifetime.cs:57-68`) — poll
  `GET /api/v1/qualityprofile` until 2 entries before provisioning.

### 7.5 Root folders via API
- Minimal bodies (`RootFolderController.cs:58-78`, `RootFolderResource.cs:171-174`):
  `{"name":"Audiobooks","path":"/audiobooks","folderType":1,
  "audiobookQualityProfileId":<id>,"audiobookMetadataProfileId":<id>}` and the ebook mirror
  with `folderType: 2`. `name` and `path` are the only hard-required fields; profile ids are
  accepted **without existence validation** (the validators are injected but never attached,
  `RootFolderController.cs:47-48`) — resolve real ids first.
- Path must exist, be readable and writable in-container (validated twice:
  `RootFolderController.cs:58-67`, `RootFolderService.cs:88-111`). Adding a root immediately
  queues `RescanFoldersCommand` on it (`RootFolderService.cs:159-161`) — mount empty dirs.

### 7.6 Readiness
- `GET /ping` is `[AllowAnonymous]`; during boot the `StartingUpMiddleware` answers **503** for
  everything, flipping to `200 {"status":"OK"}` when startup (incl. migrations) completes
  (`PingController.cs:22-44`, `StartingUpMiddleware.cs:23-51`). Fresh-DB migrations are fast
  (one big schema build; seconds, not minutes).

### 7.7 Canary hazards
- **Zero indexers → `BookSearch` is a no-op, not an error or hang**: the dispatcher awaits an
  empty task set and returns an empty decision list (`ReleaseSearchService.cs:222-297`);
  per-book exceptions are caught and logged (`BookSearchService.cs:72-80`). Assert
  acknowledgement only.
- Idle-instance outbound traffic is read-only: api2.chaptarr.com (metadata + a version/OS
  health notice), goodreads.com autocomplete, provider image CDNs, services.chaptarr.com +
  api.github.com (update check; auto-install is off on Linux, `ConfigFileProvider.cs:336`).
  No Sentry (dead code), no analytics (removed), no scheduled log uploads (manual command +
  empty token only).
- Scheduled tasks (RSS sync, import lists, Hardcover sync) are safe no-ops with nothing
  configured.
