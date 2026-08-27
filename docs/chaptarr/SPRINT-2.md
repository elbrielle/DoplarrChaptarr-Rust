# Sprint 2 Packet — Simplification & Identity

**Product:** DoplarrChaptarr-Rust · **Owner:** Elisha Lucero · **Dates:** 2026-09-03 → 2026-09-09
**Sprint goal:** The request pipeline is as small as the server's real mechanisms allow — the
settle gate rewritten around the actual scan-completion signal, roots resolved by their real
discriminators, cross-provider identity drift handled by design — and API drift fails CI before
it fails a user.

This packet is self-contained and its appendix (§7) was line-verified against Chaptarr
`v0.9.936` (`develop @ 423b1bb`) before send-off, per the process adopted after Sprint 1. Two
planned stories did not survive that verification; §6 records why, so nobody re-litigates them.

---

## 1. Working agreements (unchanged from Sprint 1; read first)

**Harness model.** Fable is the designer, manager, auditor, and final gate for anything complex,
architectural, or judgment-bearing; Opus subagents handle rudimentary work. Nothing merges
without Fable-level review of the actual diff.

**Authorship.** Everything is authored as Elisha Lucero. No AI attribution anywhere — commits,
PRs, docs, release notes.

**Engineering bar.** Root cause named in the commit message with a source citation; every fix
carries a test that fails on revert; `cargo test --workspace`, `cargo fmt --check`,
`cargo clippy --workspace --all-targets` clean (PATH needs `/opt/homebrew/opt/rustup/bin`);
radarr/sonarr/seerr untouched; comments only for non-obvious constraints, pruned in every
touched file; fixtures and docs travel with the behavior change; no band-aids.

**Git.** Branch `sprint-2-simplification` off `main` (main already contains Sprint 1 and this
packet). Local commits only; no push or PR until Elisha reviews.

**Reference source.**

```
git clone --depth 1 --branch v0.9.936 https://github.com/Chaptarr/chaptarr.git /tmp/chaptarr-ref
```

---

## 2. Scope

| # | Story | Pts | Scope |
|---|-------|-----|-------|
| 2.6 | Cross-provider identity matching for book rows | 3 | CORE |
| 3.1 | Settle gate v2 on the real scan-completion signal | 5 | CORE |
| 3.3 | Root resolution by `folderType` + nested-settings presence | 2 | CORE |
| 5.1 | Contract tests: openapi route inventory + serializer traps | 3 | CORE |
| 4.1 | Wanted-editions decision record | 1 | CORE |
| 3.4 | Code polish pass (comments, dead code, clippy) | 3 | CORE |
| 4.4 | Success-embed polish: narrator for audiobooks, series context | 2 | STRETCH |
| 6.2 | Upstream doplarr_rs sync check | 1 | STRETCH |

Committed 17 pts, stretch 3. **Cut order if squeezed:** 6.2 → 4.4 → 3.4 → 4.1. The settle gate,
identity matching, roots, and contract tests never slip — they are the sprint.

**Sequencing:** 3.1 and 2.6 are the two big lanes and are independent — run them in parallel.
3.3 and 5.1 are independent of both. 3.4 runs last across everything touched. 4.1 is a
documentation task, any time.

**Out of scope (do not touch):** adopting `PUT /author/{id}/monitor/{mediaType}` (§6.1 — it is
a landmine, not a simplification); adopting `POST /book/{id}/editions/wanted` in the pipeline
(§6.2); the live canary and beta graduation (sprint 3); the Discord interaction layer beyond
story 4.4's embed fields; radarr/sonarr/seerr.

---

## 3. Story details

### 2.6 Cross-provider identity matching (3 pts)

**Problem (top live risk from Sprint 1):** free-text lookups return `foreignBookId: "gr:<workId>"`;
once a row imports, `BuildForeignBookId` prefers `hc:` the moment `HardcoverBookId` is populated
(§7.4). `local_row_matches_item` requires exact `foreignBookId` equality, so a normalized local
row no longer matches its own lookup result — the bot would re-add or fail instead of
recognizing the existing row.

**Fix (decided):** extend book-row matching to a tiered identity chain, all read from
`BookResource`'s per-provider fields (§7.4a):

1. Exact `foreignBookId` equality (as today).
2. `goodreadsWorkId` equality — both sides canonical `"gr:<n>"`, case-insensitive compare.
3. `goodreadsBookId` equality (edition-derived `"gr:<n>"`).
4. `asin` / `audibleASIN` equality — **bare uppercase strings, no prefix** (§7.4a); normalize
   before comparing and never run these through the `prefix:value` parser.
5. Existing title-tier fallback, unchanged, only when the lookup row carries no id at all.

Hazards to encode (§7.4c): `goodreadsWorkId` on a local row can be **nulled by refresh**
(upstream-authoritative copy) — an absent field on either side simply skips that tier, never
fails the match. Authors have **no** per-provider ids on `AuthorResource` (§7.4d) — author
resolution keeps its current exact-`foreignAuthorId` + single-match-name-fallback logic; do not
attempt author-side drift matching.

**Acceptance criteria**
- A mock: lookup row `gr:work-N` matches a local row whose `foreignBookId` is `hc:M` but whose
  `goodreadsWorkId` is `gr:work-N` — request short-circuits to the existing row instead of
  re-adding.
- A mock: same match via `asin` when `goodreadsWorkId` is absent (post-refresh wipe scenario).
- Cross-format safety unchanged: an ebook row never satisfies an audiobook request through any
  tier.
- `already_requested`/pocket logic uses the same chain (one matching function, no duplicates).

### 3.1 Settle gate v2 (5 pts)

**Mechanism (verified, §7.2):** after a new-author add, the monitor-rewrite hazard is one shot:
`AuthorScannedHandler` runs when the post-add scan completes (or is skipped), bulk-rewrites the
author's book monitor flags per `AddOptions`, persists the author, then sets
`author.AddOptions = null`. Only that handler ever clears it, and `GET /author/{id}` serializes
`addOptions`. Refresh itself preserves existing rows' monitor flags, and our pin is protected.

**Fix (decided):** replace the triple-fingerprint stability gate with a composite wait:

1. `GET /author/{id}` reports `addOptions` absent (key-absent = null = handler already ran —
   works on both the scan path and the skip path), **and**
2. `GET /command` shows no `queued`/`started` `RefreshAuthor` or `RescanFolders` scoped to this
   author (`body.authorId`/`body.authorIds`; an unscoped refresh-style command still counts as
   busy, as today), **and**
3. a bounded deadline with fail-closed semantics on command/API errors (unchanged behavior).

Delete `CatalogFingerprint`, `TargetFingerprint`, `EditionFingerprint`,
`book_list_fingerprint`, and the sampling loop. Keep the write ordering exactly as it is
(settle → author gate → edition pin → `/book/monitor` → read-back verify → `BookSearch`): the
scan handler also re-persists the author (§7.2e), so the gate write must stay after settle.
For the existing-author retry path, the same composite check applies (an old author with
`addOptions` already null passes instantly — that is the point).

Two payload guards to keep/add while here (§7.2f): never send `booksToMonitor` (a
`SpecificBook` monitor type with an empty list throws server-side), and note in a comment that
`PUT /author/{id}` cannot clobber `addOptions` (server ignores it on update).

**Acceptance criteria**
- New-author mock: monitoring proceeds only after `addOptions` disappears and commands go
  quiet; a still-set `addOptions` with idle commands keeps waiting; deadline still fails closed
  with the existing user-facing message.
- Skip-path mock (author with no folder evidence): `addOptions` cleared with no `RescanFolders`
  ever appearing — gate passes.
- Fingerprint machinery deleted; `wait_for_catalog_settle` shrinks accordingly; all existing
  settle tests rewritten to the new signal, still proving the original failure scenario
  (write-before-settle) cannot happen.

### 3.3 Root resolution (2 pts)

**Fix (decided):** resolve roots in this order (§7.5): explicit configured path/name (wins, as
today, ambiguity fails closed) → `folderType` (1=Audiobook, 2=Ebook; 0=Mixed matches either) +
nested `audiobook`/`ebook` settings-object **presence** (absent = not configured for that
format) → `isEffectiveDefaultAudiobook`/`isEffectiveDefaultEbook` as tie-breakers (populated on
`GET /rootfolder`). Delete the path/name substring inference and the legacy `bool_only` guard.
Prefer the nested objects over the flattened mirror fields — the four flattened sidecar
booleans coerce null→false and the tag lists null→[], so only the nested objects distinguish
"unconfigured" from "false" (§7.5c). `accessible: false` remains never selectable.

**Acceptance criteria:** fixtures already carry the truthful shapes; resolution tests cover
folderType-typed roots, mixed roots disambiguated by nested presence, ambiguity fail-closed,
and the explicit-config override; inference code deleted.

### 5.1 Contract tests (3 pts)

Add a contract-test layer that fails CI when our narrow contract drifts:

- A checked-in route manifest (the 14 routes we depend on — all confirmed present in
  `openapi.json`, §7.6) asserted against a vendored copy of the relevant `openapi.json` paths
  section, with a small updater script that re-extracts it from a Chaptarr clone. Route
  inventory ONLY — the spec mistypes command bodies and param optionality and omits real 400s,
  so no schema codegen and no body assertions from it.
- Serializer-trap assertions promoted to a dedicated integration test over every fixture: no
  explicit nulls anywhere, no `id: 0`, no `grabbed`, no `editions` key on `/book` row fixtures,
  metadata `profileType` numeric, quality `profileType` string.

**Acceptance criteria:** removing a depended-on route from the vendored manifest fails the
test; a fixture with an explicit `null` fails the test; documented one-command refresh flow.

### 4.1 Wanted-editions decision record (1 pt)

Write `docs/chaptarr/decisions/0001-wanted-editions.md` recording the verdict (facts in §7.3):
`POST /book/{id}/editions/wanted` is **not** adopted for the request pipeline. It is
audiobook-only, requires an already-local row, never opens the author format gate, its
`searchForNewBook` search uses a non-manual trigger that `IsMonitoredWithAuthor()` silently
drops when the gate is closed, its edition re-pin failure is swallowed (logged only), and it
may return the base or a sibling row rather than the acted-on row. Our explicit
`POST /command BookSearch` is strictly stronger (manual trigger bypasses the gate filter).
Record the one future consideration: an audiobook-narrator feature could revisit it. Add one
line to RELEASE_CHECKLIST's canary list: probe the endpoint once to confirm live behavior
matches this reading.

### 3.4 Polish pass (3 pts)

Across the provider (`chaptarr.rs`, `selection.rs`, `models.rs`): prune comments to
constraint-explaining only (several still narrate history or restate code); remove dead code
exposed by 3.1's deletions; collapse any now-single-use helpers; run clippy with
`--all-targets` plus a judgment pass for readability. No behavior changes — this commit should
be provably refactor-only (tests unchanged and green).

### 4.4 STRETCH — success-embed polish (2 pts)

After our edition pin, `narrator`/`narratorNames` become visible on audiobook rows (the server
gates them on files-or-pinned-edition, §7.3 note). Show narrator in the audiobook success
embed, and series context (`seriesTitle`) when present. Read from the post-verify book state we
already fetch — no new requests.

### 6.2 STRETCH — upstream sync check (1 pt)

Fetch `upstream` (activexray/doplarr_rs), report what main has diverged on, and either merge
cleanly or write up what a merge would take. No merge without a clean, test-green result.

---

## 4. Definition of done (sprint level)

- [ ] Core stories merged to `sprint-2-simplification`, each meeting §1's bar.
- [ ] Full workspace suite green; fmt + clippy clean; refactor-only commits provably
      behavior-neutral.
- [ ] Demo: new-author request against the mock passes the composite settle gate (including
      the skip path), and a drifted-identity request (`gr:` lookup vs `hc:` local row)
      short-circuits to the existing row.
- [ ] COMPATIBILITY.md updated where behavior changed (settle §, roots §, identity §);
      CHANGELOG entry; decision record 0001 committed.
- [ ] Write-path-affecting changes appended to RELEASE_CHECKLIST's sprint-3 canary list.
- [ ] Closing summary for Elisha: merged/cut/learned, and anything that reshapes sprint 3.

---

## 5. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| `addOptions` observability differs live from source reading | Settle gate passes early | Command-quiet check remains ANDed; deadline fails closed; canary list gets an explicit new-author settle probe (already present from Sprint 1) |
| Identity chain accidentally loosens matching | Wrong book monitored | Chain only runs inside format-matched rows; each tier is exact-equality on canonical ids; title fallback unchanged and last |
| Fingerprint deletion drops a protection nobody documented | Regression on live import | The original 2026-07-15 failure scenario stays as a test against the new gate; canary re-runs new-author settle |
| Route-manifest test is brittle to server releases | CI noise | Manifest is OURS (14 routes); it changes only when our contract changes — the updater script just refreshes the vendored openapi extract |

---

## 6. Cut stories — do not resurrect without new evidence

### 6.1 ~~3.2 Author gate via `PUT /author/{id}/monitor/{mediaType}`~~ — REFUTED, landmine

Verification showed this endpoint does **not** touch the author entity at all: it bulk-rewrites
the per-book media-type monitor flag for **every book of the author** via raw SQL, returns bare
200, and leaves `ebookMonitorFuture`/`audiobookMonitorFuture`/`monitorExisting` unchanged
(§7.1). Calling it would clobber unrelated books' monitor state and still leave automatic
search gated off. The author gate keeps its current implementation: full-resource
`PUT /author/{id}` setting the format's `*MonitorFuture` + `monitored`, verified by re-read.
Document the endpoint as a landmine in COMPATIBILITY.md while in there (3.1 touches that
section).

### 6.2 ~~4.1 as a spike~~ — resolved from source

The spike's investigation half is done (§7.3); what remains is the decision record (story 4.1)
and one canary probe line. Do not build a pipeline variant on this endpoint this sprint.

---

## 7. Appendix — verified source facts (Chaptarr `develop @ 423b1bb`, tag v0.9.936)

### 7.1 The author-monitor endpoint (landmine)
- Route `PUT /api/v1/author/{id}/monitor/{mediaType}` (`AuthorController.cs:1154-1166`), body
  `{"monitored": bool}` (`MonitoringResource.cs:3-6`), mediaType `audiobook|ebook` only
  (`MediaTypeParameterParser.cs:9-40`).
- Effect: `AuthorService.cs:1147-1153` → `BookRepository.cs:679-716` — raw SQL UPDATE of
  `AudiobookMonitored`/`EbookMonitored` across **all** the author's book ids. No author fields
  change; response is 200 with empty body; unknown author → 404 after the silent no-op update.
- The automatic-search gate remains `MonitorExisting > 0 || MonitorFuture == true`
  (`AuthorExtensions.cs:16-39`), moved only by full-resource `PUT /author/{id}`.

### 7.2 Settle mechanism
- a. `AuthorScannedHandler.cs:36-56`: gate on `AddOptions != null` (`:41`), bulk rewrite
  (`:43`), optional `MissingBookSearchCommand` (`:45-48`), `AddOptions = null` + persist
  (`:50-51`), `SearchForRecentlyAdded` (`:54`). Handles both `AuthorScannedEvent` and
  `AuthorScanSkippedEvent` (`:64-67`).
- b. `GET /author/{id}` serializes `addOptions` (`AuthorResource.cs:81`, mapped `:159`); only
  the handler clears it; `PUT /author/{id}` cannot overwrite it (`AuthorResource.cs:571-573`,
  `AuthorService.cs:634-636`).
- c. Command sequence: `RefreshAuthor {isNewAuthor:true,...}` (pushed by
  `AuthorAddedHandler.cs:35`) → `RescanFolders` pushed from inside refresh
  (`RefreshAuthorService.cs:2185-2191`); `AuthorScannedEvent` is raised only by
  `DiskScanService.cs:892-896` under the `RescanFolders` execution. Skip path: no folder
  evidence → `AuthorScanSkippedEvent` inside RefreshAuthor (`RefreshAuthorService.cs:2170-2178`),
  no `RescanFolders` appears. `DownloadedBooksScan` is not part of this flow.
- d. `book.addOptions` is never null (`Book.cs:30` constructor default) — useless as a signal.
  `AuthorScanCompletedEvent` has zero consumers — not observable.
- e. The rewrite also re-persists the author (`BookMonitoredService.cs:118-125`), so author
  writes must stay after settle.
- f. `MonitorTypes.SpecificBook` with empty `booksToMonitor` throws server-side
  (`BookMonitoredService.cs:113-114`); never send `booksToMonitor`.
- g. Refresh preserves existing rows' monitor flags: `Book.UseDbFieldsFrom` at
  **`Book.cs:337-338`** (also preserves `AnyEditionOk` `:339`, `AddOptions` `:343`); new rows
  get monitored only under `MonitorExisting == 1` (All) — `RefreshAuthorService.cs:1804-1805`.
  Pin protection: `EditionPinPolicy.cs:20`; pinned books un-prunable
  `RefreshBookService.cs:428-447` (guard consumed at `:414-425`).

### 7.3 Wanted-editions endpoint
- `POST /api/v1/book/{id}/editions/wanted` (`BookController.cs:1957-1989`), body
  `{editionId: int, searchForNewBook: bool=false}` (`AddWantedEditionRequest.cs:3-10`).
- `BookService.AddWantedEdition` (`BookService.cs:2094-2392`): audiobook-only (`:2102-2105`);
  requires local book + edition (`:2096-2116`); no-files branch pins in place
  (`:2119-2146`); dedupe branch may return an existing sibling (`:2148-2246`); create branch
  makes a `_wanted_` row with `AddType=Manual` and re-pins with a swallowed-on-failure write
  (`:2383-2388`).
- Never touches author gates or profiles. Its queued `BookSearchCommand` uses a non-manual
  trigger → filtered by `IsMonitoredWithAuthor()` (`BookSearchService.cs:168-181`); an explicit
  `POST /command` gets `Trigger = Manual` (`CommandController.cs:114`) and bypasses that filter.
- Narrator gating note (for 4.4): `BookResource.cs:249-256` — narrator fields shown only with
  files or a pinned edition (`!AnyEditionOk`).

### 7.4 Identity fields
- a. `BookResource` per-provider ids (all string, null-omitted): `hardcoverBookId` (`:36`/`:199`),
  `goodreadsBookId` — edition-derived `"gr:<n>"` (`:37`/`:200`, `BookEditionIdentity.cs:127-138`),
  `goodreadsWorkId` (`:38`/`:201`), `openLibraryWorkId` (`:39`/`:202`), `googleBooksId`
  (`:40`/`:203`), `asin` and `audibleASIN` — **bare, uppercased, unprefixed**
  (`:41-42`/`:204-205`, `BookEditionIdentity.cs:533-541`).
- b. Free-text lookup rows set **only** `goodreadsWorkId` (+ edition-level `goodreadsBookId`,
  author `gr:` id): `BookInfoProxy.cs:3767,3777,3794-3795`.
- c. `foreignBookId` flips `gr:`→`hc:` when `HardcoverBookId` populates
  (`BookResource.cs:798-871` precedence). Refresh copies provider ids upstream-authoritatively:
  `Book.cs:286-288` + `CleanProviderIdForCopy` `:318-331` — a blob missing `goodreadsWorkId`
  **wipes it to null** on the next changed refresh. `RemoteProviderIds` (the alias set) is not
  serialized — invisible to clients.
- d. `AuthorResource` has **no** per-provider id fields — only the computed `foreignAuthorId`
  (`AuthorResource.cs:27`, `BuildForeignAuthorId` `:195-222`, same `hc:`-preferred drift).

### 7.5 Root folders
- `folderType`: int enum `Mixed=0, Audiobook=1, Ebook=2` (`RootFolder.cs:9-14`, resource `:39`,
  mapped `:103`); write-validated with 400 on bad values (`RootFolderResource.cs:212-216`);
  cross-format settings rejected per type (`:274+`, `:386-395`).
- Nested `audiobook`/`ebook` objects: null (key absent) when unconfigured
  (`RootFolderResource.cs:397-411`, `RootFolder.cs:65-93`), populated `:402-410`.
- c. Flattened mirrors are null-safe **except** the four sidecar booleans (coerce `?? false`,
  `:124-127`) and the two tag lists (coerce to `[]`, `:130-131`) — nested objects are the
  null-vs-false source of truth.
- `isEffectiveDefault*` populated on `GET /rootfolder` and `GET /rootfolder/{id}`
  (`RootFolderController.cs:177-184`, `:108-112`; resolver `RootFolderResource.cs:139-161`).
  From other mappers they default to `false` = "not computed".

### 7.6 Openapi route inventory
All 14 depended-on routes present in `src/Chaptarr.Api.V1/openapi.json` (236 paths):
`/api/v1/system/status`, `/book/lookup`, `/author`, `/author/{id}`, `/book`, `/book/{id}`,
`/book/monitor`, `/edition`, `/command`, `/qualityprofile`, `/metadataprofile`, `/rootfolder`,
`/author/{id}/monitor/{mediaType}`, `/book/{id}/editions/wanted`. Known spec lies: command
`body` undertyped, param optionality wrong, controller 400s undocumented — route inventory
only, never codegen.
