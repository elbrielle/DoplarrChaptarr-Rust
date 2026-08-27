# Sprint 1 Packet — Truth & Correctness

**Product:** DoplarrChaptarr-Rust · **Owner:** Elisha Lucero · **Dates:** 2026-08-27 → 2026-09-02
**Sprint goal:** Every decision the bot makes is grounded in Chaptarr's actual source contract: fixtures and the version line rebaselined to Chaptarr 0.9.936, and the three known correctness defects fixed at the root — each with a test that fails if the fix is reverted.

This packet is self-contained. It folds in the findings of the 2026-08-26 source reconciliation
(DoplarrChaptarr-Rust `4.6.1-chaptarr.1` vs. Chaptarr `develop @ 423b1bb`, tag `v0.9.936`), so no
external report is required to execute it.

---

## 1. Working agreements (read first)

**Harness model.** Fable is the designer, manager, auditor, and final gate for anything complex,
architectural, or judgment-bearing. Rudimentary work — mechanical edits, fixture generation, test
scaffolding, repo searches — is delegated to Opus subagents. Nothing merges without Fable-level
review of the actual diff.

**Authorship.** Everything is authored as Elisha Lucero. No `Co-Authored-By: Claude` trailers, no
"Generated with Claude Code" footers, no AI attribution in commits, PRs, docs, or release notes.

**Engineering bar.**
- Every fix names its root cause in the commit message, citing the Chaptarr source mechanism it
  addresses (file paths from the appendix, §7).
- Every fix carries a test that fails when the fix is reverted.
- `cargo test` (whole workspace), `cargo fmt --check`, `cargo clippy` clean before any commit is
  considered done. The radarr/sonarr/seerr backends must not change behavior.
- Comments only where they explain a non-obvious constraint. Prune stale comments in every file
  touched — especially comments asserting server behavior this sprint disproves.
- Fixtures and docs travel in the same commit as the behavior change they describe.
- No band-aids: if a defense can't be tied to a mechanism in Chaptarr source, delete it rather
  than keep it "just in case."

**Git.** Branch `sprint-1-truth-correctness` off `main`. Commit per story (or coherent story
pair). Local commits only; no push or PR until Elisha reviews the sprint result.

**Reference source.** Clone Chaptarr for citation-checking (read-only reference, never vendored):

```
git clone --depth 1 --branch v0.9.936 https://github.com/Chaptarr/chaptarr.git /tmp/chaptarr-ref
```

Reconciled ref: `develop @ 423b1bb`, tag `v0.9.936`. All appendix citations are against this ref.

---

## 2. Scope

| # | Story | Pts | Scope |
|---|-------|-----|-------|
| 1.1 | Fixture rebaseline to 0.9.936 serializer truth | 5 | CORE |
| 1.2 | Version line + `--check` preflight rebaseline | 2 | CORE |
| 2.1 | Edition discriminator → `readingFormatId` | 3 | CORE |
| 2.2 | Retire placeholder gating; relax `book_complete` | 3 | CORE |
| 2.3 | Remove `grabbed`-based detection | 2 | CORE |
| 1.3 | COMPATIBILITY.md v2 with source citations | 3 | CORE |
| 2.4 | `POST /book` 202-pending / ambiguity handling | 3 | STRETCH |
| 2.5 | Provider-ID round-trip contract test | 2 | STRETCH |

Committed 18 pts, stretch 5. **Cut order if squeezed:** 2.5 → 2.4 → 1.3 slips to sprint 2. Code
fixes never slip behind the doc.

**Sequencing:** 1.1 + 1.2 first (everything else tests against the new fixtures). Then 2.1 and
2.2 sequentially on one lane (they touch the same selection code), 2.3 in parallel. 1.3 last —
it documents what actually shipped. Stretch only after all core is merged.

**Out of scope this sprint (do not touch):** the catalog-settle gate rewrite (sprint 2), root-
folder resolution changes (sprint 2), `POST /book/{id}/editions/wanted` (sprint 3 spike), the
Discord layer, and the radarr/sonarr/seerr providers.

---

## 3. Story details

### 1.1 Fixture rebaseline (5 pts)

Regenerate every fixture in `doplarr/tests/fixtures/chaptarr/` to match Chaptarr 0.9.936's real
serializer. The serializer's global rules (appendix §7.1) change fixture shape materially:

- Null properties are **omitted** — a fixture must never contain an explicit `null` value.
- `id: 0` is **omitted** (`RestResource.Id` is `[JsonIgnore(WhenWritingDefault)]`). Non-local
  lookup rows therefore have **no `id` key at all** — the same applies to nested `author.id` and
  `editions[].id` on remote results.
- `grabbed: false` is omitted; `grabbed` effectively never appears in REST responses (§7.4).
- No `default-*` `foreignEditionId` values exist anymore (§7.5) — remove them from fixtures.
- Book GET responses (`/book`, `/book/{id}`) never carry an `editions` key — absent, not `[]`.
  Only `/book/lookup` and `/search` responses include `editions` (§7.3).
- Enums serialize as camelCase strings — with two exceptions that stay numeric because the
  resource property is declared `int`: `metadataprofile.profileType` (0=General, 1=Audiobook,
  2=Ebook) and `rootfolder.folderType` (0=Mixed, 1=Audiobook, 2=Ebook) (§7.6).
- `qualityprofile.profileType` is the string `"audiobook"` / `"ebook"` (no none/general value).
- Root folders: the nested `ebook` / `audiobook` settings objects are present **only when the
  root is configured for that format** (absent otherwise); flat per-format mirror fields
  (`ebookQualityProfileId`, …) sit alongside them (§7.7).
- Editions carry `readingFormatId` (1=physical, 2=audio, 3=ebook), `isEbook` (bool), and a
  free-text `format` (provider text like "Kindle Edition" — NOT an enum) (§7.5).
- Free-text lookup results: `gr:`-prefixed foreign IDs, `mediaType: "audiobook"` (a C# struct
  default — every free-text result gets it), no `isbn13`/`asin` (V5-path only), relative proxied
  image URLs (`/MediaCoverProxy/…`) with the absolute upstream URL only in `remoteCover` (§7.8).

Also add a **serializer-traps test module**: our models must deserialize key-absent shapes for
`id`, `grabbed`, `editions`, monitor gates, and nested root settings without error.

**Acceptance criteria**
- All fixtures conform to the rules above; every existing contract test updated and green.
- Serializer-traps tests exist and pass.
- Fixture provenance annotations updated (final table lands with 1.3).

### 1.2 Version line + `--check` (2 pts)

`TESTED_CHAPTARR_VERSION` in `doplarr/src/providers/chaptarr/selection.rs` is `"0.9.720"` — 216
patch versions stale. Pin to `"0.9.936"` (same starts-with pattern). Update `system_status.json`,
the `--check` preflight expectations, and `.github/ci/smoke-config.toml` if it embeds a version.
Untested-version warning behavior is unchanged — only the baseline moves.

**Acceptance criteria**
- `--check` against 0.9.936-shaped fixtures reports supported; a different version still warns.
- CI smoke passes.

### 2.1 Edition discriminator → `readingFormatId` (3 pts)

**Defect:** `edition_usable()` (and everything downstream: `preferred_edition_index`,
`sole_monitored_edition`, `usable_edition_count`, read-back verification) matches the free-text
`format` field against `"ebook"`/`"audiobook"`. In source, `Edition.Format` is verbatim provider
text; the structured discriminators are `readingFormatId` and `isEbook` (§7.5). An ebook edition
labeled "Kindle Edition" (or with an empty `format`) is wrongly rejected → spurious "no usable
edition" failures.

**Fix (decided, not open for redesign):** discriminate by `readingFormatId` first (2=audio,
3=ebook; 1=physical never matches either request format). When `readingFormatId` is absent, fall
back to `isEbook` (`true`→ebook, `false` alone is NOT audiobook-proof — it can be physical, so
`false`/absent without `readingFormatId` fails closed for writes). `format` text becomes
display/logging only. Read-only ranking (`edition_projection_compatible`) may stay tolerant, but
write-path usability and read-back verification use the strict rule.

**Acceptance criteria**
- `readingFormatId: 3` + `format: "Kindle Edition"` → usable for ebook.
- `readingFormatId: 1` → never usable for either format.
- No `readingFormatId`, `isEbook: true` → usable for ebook; `isEbook: false`/absent → not usable
  for audiobook (fails closed).
- Read-back (`sole_monitored_edition` path) uses the same discriminator; model added for
  `readingFormatId` in `models.rs`.

### 2.2 Retire placeholder gating; relax `book_complete` (3 pts)

**Defect:** `book_complete()` requires `releaseDate`, non-empty `images`, and a
`foreignEditionId` not starting with `default-`. The `default-` placeholder mechanism no longer
exists in Chaptarr (§7.5: no such IDs are minted; `IsFallbackEdition` is a dead flag). Missing
release date/images is now just sparse upstream metadata — the gate can block legitimate
requests. The guarded `RefreshAuthor` repair path exists only to resolve those placeholders, and
refresh has real deletion side effects (§7.9), so it must go with them.

**Fix:** delete the `default-` check and the images/releaseDate completeness requirements;
delete `needs_author_refresh()` and the `RefreshAuthor` branch in `request()`; simplify
`poll_target()` accordingly (row presence + identity match, not "completeness"). Keep every
identity guard unchanged: positive local id, `foreignBookId` match, title-tier fallback,
format-bound row matching, multi-book rejection.

**Acceptance criteria**
- A sparse-metadata row (no images, no release date) flows through edition selection and
  monitoring in mock tests.
- No code path in the provider issues `RefreshAuthor`.
- Identity/junk/multi-book guards demonstrably unchanged (existing tests still pass).

### 2.3 Remove `grabbed`-based detection (2 pts)

**Defect:** `grabbed` is `[JsonIgnore(WhenWritingDefault)]`, always set `false` on the REST
mapping path, and only ever `true` on the SignalR broadcast path (§7.4) — so our
`request_state_across()` grab check is dead code that can never fire.

**Fix:** remove `grabbed` from `BookShape` usage and the state machine. In-flight detection =
files present, an active exact `BookSearch` command, or the in-process ack cache — which is what
actually fires today.

**Acceptance criteria:** no `grabbed` reference remains in the provider; state-machine tests
updated; behavior otherwise identical.

### 1.3 COMPATIBILITY.md v2 (3 pts)

Rewrite `docs/chaptarr/COMPATIBILITY.md` so every claim cites Chaptarr source (paths from §7)
instead of live-probe folklore. Must cover: the serializer traps; the lookup `mediaType` landmine
(free-text + `mediaType=ebook` → empty array — we never send it); pockets as intentional design;
the two PUT routes and their differing pin semantics; what `PUT /book/{id}` actually persists
(top-level `monitored` → media-typed column; per-format body flags ignored on update; top-level
`Book.Monitored` is a dead column — the origin of our old misreading); the full author monitor
model (`*MonitorFuture`, tri-state `*MonitorExisting`, `syncMonitoredAcrossFormats`); the
profileType string/int asymmetry; `POST /book` hard edges (§7.10); and the updated fixture
provenance table. Keep the drift policy, including "never codegen from openapi.json" — now with
the honest rationale: the spec exists but mistypes command bodies and param optionality; route
inventory only. Baseline: `v0.9.936 (develop @ 423b1bb)`. Add a CHANGELOG entry.

**Acceptance criteria:** no unsourced behavioral claim remains; every § cites a source path;
provenance table matches the shipped fixtures.

### 2.4 STRETCH — `POST /book` response branches (3 pts)

The add endpoint has three response shapes (§7.10): `201` with a full `BookResource`; `202` with
`PendingBookRequestResource { pendingId, message }` when upstream metadata is unavailable
(currently we'd treat it as a generic acknowledgement); and a custom provider-ambiguity status
with `ProviderAmbiguityResource`. Handle each: 202-pending → user-facing "Chaptarr queued this
work upstream — try the request again in a few minutes"; ambiguity → actionable message naming
the conflict. Mock tests per branch.

### 2.5 STRETCH — provider-ID round-trip contract test (2 pts)

`POST /book` 400s unless an upstream provider work ID survives mapping
(`IsMissingUpstreamProviderBookId`, §7.10), and native-API IDs must be prefixed
(`hc:`/`gr:`/`ol:`/`gb:`/`az:`). Add a contract test proving our allowlisted add bodies retain
what the server's `BookResource.ToModel` needs; if the allowlist strips a required field, fix the
body (that finding would also feed 1.3).

---

## 4. Definition of done (sprint level)

- [ ] All core stories merged to `sprint-1-truth-correctness`, each satisfying §1's bar.
- [ ] Full workspace test suite green; fmt + clippy clean.
- [ ] Demo scenario passes: a sparse-metadata ebook request (mock server) resolves, selects an
      edition via `readingFormatId`, monitors, verifies, and queues `BookSearch`.
- [ ] COMPATIBILITY.md v2 + CHANGELOG updated (unless cut per §2).
- [ ] Write-path-affecting changes are listed for the sprint-3 live canary; nothing claims beta
      graduation this sprint.
- [ ] Summary for Elisha: what merged, what was cut and why, anything discovered that changes
      sprint 2.

---

## 5. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Relaxing `book_complete` was masking a real failure | Bad rows monitored | Identity guards stay strict; read-back verification unchanged; only images/date/placeholder checks are removed |
| 2.1 + 2.2 touch the same selection code | Conflicts/churn | One lane, sequential, reviewed as one narrative |
| Fixture rebaseline breaks many tests at once | Long red period | Land 1.1 as fixture+test-update pairs per endpoint, not one big bang |
| Chaptarr moves again mid-sprint | Stale citations | All citations pinned to `423b1bb`; drift handling is sprint-3 scope |

---

## 6. What we deliberately keep (do not "improve" these away)

- Pocket disambiguation and cross-row state checks — pockets are Chaptarr's intentional data
  model (work × media type = separate Book rows; only bit-identical pockets coalesce).
- The edition pin: full-book `PUT /book/{id}` with `anyEditionOk: false` + exactly one
  `monitored: true, manualAdd: true` edition — the officially supported, refresh-surviving pin.
- The verified read-back sequence, per-work mutation locks, bounded search-ack dedup.
- Client-side junk/multi-book filtering (the lookup path has zero server-side filtering).
- Never sending `mediaType` on free-text lookup; retaining cross-format projections.
- The narrow handwritten client (no codegen from openapi.json).
- OpenLibrary cover enrichment (free-text lookups carry no ISBN/ASIN; images are proxied).

---

## 7. Appendix — source facts (Chaptarr `develop @ 423b1bb`, tag v0.9.936)

### 7.1 Serializer globals
- `src/NzbDrone.Common/Serializer/System.Text.Json/STJson.cs:26` — `WhenWritingNull`: null
  properties omitted entirely. `:33` — `JsonStringEnumConverter(camelCase)` for all C# enums.
  `:31` — responses are pretty-printed. Request parsing is case-insensitive.
- `src/Chaptarr.Http/REST/RestResource.cs:7-8` — `Id` is `int` with `WhenWritingDefault`:
  omitted when 0, never a string, never null.

### 7.2 Lookup
- `src/Chaptarr.Api.V1/Books/BookLookupController.cs:16-90` — free-text terms go to Goodreads
  autocomplete only; provider-prefixed terms (`hc:|gr:|ol:|gb:|az:|isbn:`) resolve locally;
  `:85-88` filters remote results by `mediaType` — combined with the struct default below,
  `term=<text>&mediaType=ebook` returns `[]`.
- `src/NzbDrone.Core/Books/Model/Book.cs:100` — `MediaType` defaults to `Audiobook` (=0);
  the free-text mapper (`BookInfoProxy.cs:3757-3826`) never assigns it → every free-text result
  is an "audiobook" projection. Per-format projections exist only on the provider-ID path
  (`BookInfoProxy.cs:5274-5287`).
- `BookLookupController.cs:223-230` — `localEbookBooks`/`localAudiobookBooks` population; the
  top-level `id` on remote rows stays 0 (omitted).
- Cover URLs: proxied relative (`MediaCoverService.cs:405-414`); `remoteCover` holds the
  absolute upstream URL (`BookLookupController.cs:228-236`).

### 7.3 Book resources and editions
- `src/Chaptarr.Api.V1/Books/BookResource.cs:137-259` — the GET mapper never assigns
  `Editions`; repo-wide, `resource.Editions` is set only in `BookLookupController.cs:235` and
  `SearchController.cs:173`. `/edition?bookId=N` (`EditionController.cs:29-30`, repeatable
  param) is the only edition read for stored books.
- `BookResource.cs:212-231` — `title`/`overview`/`images`/`ratings`/`releaseDate` come from the
  monitored edition when one exists.

### 7.4 Monitoring
- `BookResource.cs:983-991` (PUT `ToModel`) — `resource.Monitored` is projected onto the stored
  row's media-typed column (`AudiobookMonitored`/`EbookMonitored`); the per-format body flags are
  never read on update. Top-level `Book.Monitored` is a dead legacy column (`Book.cs:105-107`).
- Two PUT routes with different semantics — `BookController.cs:1791` (`PUT /book`, body-id,
  `pinExplicitEditionChange: false`) vs `:1797` (`PUT /book/{id}`, `true`). The `{id}` route
  forces `AnyEditionOk = false` on an explicit single-monitored-edition change (`:1827-1830`).
  Use `/book/{id}` with the full editions array.
- `BookController.cs:1948-1955` — `PUT /book/monitor` `{bookIds, monitored}`; enforces the
  cross-format mutual-exclusion invariant (`Book.cs:385-399`).
- `grabbed`: `BookResource.cs:88` `WhenWritingDefault` + `:236` always `false` on REST; only the
  SignalR path sets it (`BookController.cs:1997`).
- Author gates: `AuthorResource.cs:53-61` — `{ebook,audiobook}MonitorFuture` (`bool?`),
  tri-state `{ebook,audiobook}MonitorExisting` (`int?`), `syncMonitoredAcrossFormats`. Gate is
  ANDed into search at SQL level (`AuthorExtensions.cs:16-93`). A missing quality profile for
  the media type independently empties searches (`ReleaseSearchService.cs:106-113`).
- `EditionPinPolicy.cs:20` — `ManualAdd || (!AnyEditionOk && Monitored)` editions are protected
  from automation re-picks; such books are un-prunable (`RefreshBookService.cs:440-443`).

### 7.5 Editions and "placeholders"
- `src/NzbDrone.Core/Books/Model/Edition.cs:41` — `Format` is a free string (provider text).
  `:53-57` — `ReadingFormatId`: 1=physical, 2=audio, 3=ebook; plus `IsEbook`, `EditionFormat`.
- No `default-*` foreignEditionIds are minted anywhere; `Edition.IsFallbackEdition`
  (`Edition.cs:89`) is never set true. Blank-title editions are rejected on insert
  (`EditionService.cs:235-243`). Sparse `releaseDate`/`images` is ordinary upstream data.

### 7.6 Profiles
- `QualityProfile.cs:9-13` — `ProfileType { Audiobook = 1, Ebook = 2 }`, serialized as the
  camelCase string via the resource enum property.
- `MetadataProfileResource.cs:14,83` — `ProfileType` declared `int` (explicit cast), so it stays
  numeric: `0=General, 1=Audiobook, 2=Ebook`.

### 7.7 Root folders
- `RootFolderResource.cs:46-47, 399-400` — nested `ebook`/`audiobook` settings objects are null
  (omitted) when the root isn't configured for that format; presence = configured. `folderType`
  is `int` (0=Mixed, 1=Audiobook, 2=Ebook). Flat per-format mirror fields at `:50-69`.
  (Resolution changes are sprint-2 scope; fixtures must be shape-correct now.)

### 7.8 Identity
- `ProviderIdHelper.cs:9-19` — canonical prefixes `hc, gr, ol, gb, az`; preference order
  `hc > gr > ol > gb > az` (`BookResource.cs:798-870`, `AuthorIdentity.cs:40-44`). Free-text
  results carry `gr:` IDs; imported entities normalize to `hc:` — the drift is structural, and
  primary IDs are documented as mutable (`docs/API_IDENTITY_AND_LIFECYCLE.md:13-46`).

### 7.9 Refresh / settle (context only — sprint-2 scope)
- The one-shot revert: `AuthorScannedHandler.cs:41-52` bulk-rewrites monitor flags at scan
  completion while `author.AddOptions` is still set, then clears it. Refresh preserves existing
  rows' monitor flags (`Book.cs:322-324`) and re-derives edition selection while honoring pins.
  Metadata-profile filtering during refresh can delete unprotected rows
  (`RefreshAuthorService.cs:568-622`, `RefreshBookService.cs:414-447`).

### 7.10 POST /book edges
- `BookController.cs:1175-1384` — `mediaType` from query or body; both-formats Seerr-compat path
  when neither is supplied (we always send it).
- `:1386-1397` — 400 unless an upstream provider work ID survives mapping
  (`IsMissingUpstreamProviderBookId`); `:1399+` — bare (unprefixed) foreign IDs rejected on the
  native API.
- Legacy singular `qualityProfileId`/`metadataProfileId` are translated by profile type when
  per-format fields are absent (`:1613-1663`) — our four per-format IDs remain the better
  payload. Per-format profiles live on the nested `author`, not the book resource.
- Responses: `201` full `BookResource`; `202` `PendingBookRequestResource {pendingId, message}`
  when upstream is unavailable; custom ambiguity status with `ProviderAmbiguityResource`.
- `openapi.json` (`src/Chaptarr.Api.V1/`) is committed but mistypes `CommandResource.body` and
  param optionality — route inventory only, never codegen.
