# Chaptarr compatibility contract (v2)

This document defines the deliberately small part of Chaptarr's API that
DoplarrChaptarr depends on. Chaptarr is pre-1.0 and its committed OpenAPI
document is unreliable (see the drift policy), so the Rust client is
handwritten, tolerant of additive fields, and tested against fixtures shaped by
the serializer rules below.

**Baseline: Chaptarr `v0.9.936` (`develop @ 423b1bb`).** Every behavioral claim
in this document cites a file path in the Chaptarr source at that ref. Claims
that earlier revisions of this document attributed to live probing have been
re-derived from source; where the source contradicted the folklore, the source
won. Operational timing observations (for example, how long a large catalog
import takes) are labeled as observations.

The base URL is `<CHAPTARR_URL>/api/v1`. Send the API key in `X-Api-Key`; never
put it in a URL, log line, fixture, or Discord response.

## Serializer contract (the traps)

All REST responses pass through one serializer configuration
(`src/NzbDrone.Common/Serializer/System.Text.Json/STJson.cs:27-33`, wired into
MVC in `src/NzbDrone.Host/Startup.cs:99-101`):

- **Null properties are omitted entirely** (`WhenWritingNull`). A response
  never contains an explicit `null`; the key is simply absent. Fixtures must
  never contain a `null` value.
- **`id: 0` is omitted.** `RestResource.Id` is an `int` with
  `[JsonIgnore(WhenWritingDefault)]` (`src/Chaptarr.Http/REST/RestResource.cs:7-8`).
  A non-local lookup row therefore has **no `id` key at all** — and an id is
  never a string and never null. A string id is malformed data and fails
  closed.
- **`grabbed` is effectively never present.** `BookResource.Grabbed` and
  `EditionResource.Grabbed` carry `WhenWritingDefault`
  (`BookResource.cs:87-88`, `EditionResource.cs:65`), and the REST mapping
  hardcodes it `false` (`BookResource.cs:236`). Only the SignalR broadcast
  handler sets it (`BookController.cs:1997`). A REST client can never observe
  `grabbed: true`, so nothing may key on it.
- **Book GET responses never carry an `editions` key.** The `ToResource`
  mapper (`BookResource.cs:142-259`) never assigns `Editions`; repo-wide, the
  property is set only by `BookLookupController.cs:235` and
  `SearchController.cs:173`. On `/book` and `/book/{id}` the key is absent —
  not `[]`.
- **Enums serialize as camelCase strings** (`JsonStringEnumConverter`,
  `STJson.cs:33`) — except where a resource declares the property as `int`
  (see the profileType asymmetry below).
- **Plain `bool`/`int` properties always serialize**, `false`/`0` included
  (only `WhenWritingNull` is global). Nullable properties (`int?`, `bool?`,
  `string`) are omitted when null. `readingFormatId` is `int?`
  (`EditionResource.cs:48`) and is omitted when unset.
- Request parsing is case-insensitive; responses are pretty-printed. Neither
  matters to the client, but explains payload tolerance on the server side.

Our models must deserialize every key-absent shape above without error; the
`serializer_traps` test module in `doplarr/src/providers/chaptarr/models.rs`
enforces this.

## Supported endpoints

| Method | Endpoint | Purpose | Required response data |
| --- | --- | --- | --- |
| `GET` | `/system/status` | Startup compatibility check | `appName: "Chaptarr"`, non-empty `version` |
| `GET` | `/book/lookup?term=...` | Read-only search before confirmation | title, author identity, foreign work identity; images/editions when present |
| `GET` | `/author` | Resolve an already-local author | `id`, `foreignAuthorId`, `authorName` |
| `GET` | `/author/{id}` | Read and verify the requested-format author gate | `id`, `ebookMonitorFuture`, `audiobookMonitorFuture` |
| `GET` | `/book?authorId=...` | Poll and rank local format rows | fields listed under "Book rows" |
| `GET` | `/book/{id}` | Verify status and monitor writes; carries no `editions` key | fields listed under "Book rows" |
| `GET` | `/edition?bookId=...` | The only source of local edition truth | `id`, `readingFormatId`/`isEbook`, `monitored`; language, title, identifiers when present |
| `GET` | `/command` | Detect in-flight catalog commands while an import settles | `name`, `status`; `body.authorId`/`body.authorIds` when present |
| `GET` | `/qualityprofile` | Resolve per-format quality profiles | `id`, `name`, string `profileType` |
| `GET` | `/metadataprofile` | Resolve per-format metadata profiles | `id`, `name`, numeric `profileType` |
| `GET` | `/rootfolder` | Resolve accessible roots | `id`, `path`; `accessible`, `folderType`, nested settings when present |
| `POST` | `/book` | Add a new author/catalog or an exact work under an existing author | see "POST /book hard edges" |
| `PUT` | `/author/{id}` | Enable one format's author-level monitor gate | response is not trusted; verify with `GET` |
| `PUT` | `/book/{id}` | Select exactly one edition via the complete book body | response is not trusted; verify with `GET /edition?bookId=...` |
| `PUT` | `/book/monitor` | Monitor one selected book row (the only book-level monitor write that persists) | HTTP success only; verify with `GET /book/{id}` |
| `POST` | `/command` | Queue one `BookSearch` | command acknowledgement; the body is otherwise opaque |

The provider no longer issues `RefreshAuthor` under any circumstance (see
"Sparse metadata is not a placeholder").

Unknown response fields must be ignored. Fields used only for ranking or
covers must be optional. A missing identity is not optional. A format
discriminator and local row ID become mandatory when resolving the exact local
row before a write; they are not required merely to display a lookup result.

## Lookup

### Free-text lookups are Goodreads-autocomplete projections

Free-text terms go to Goodreads autocomplete only; provider-prefixed terms
(`hc:|gr:|ol:|gb:|az:|isbn:`) resolve through a different local/provider path
(`src/Chaptarr.Api.V1/Books/BookLookupController.cs:45-94`, prefix resolution
at `:96-215`; the proxy routes canonical prefixes to `SearchByV5WorkId`,
`BookInfoProxy.cs:2475-2499`). The bot only ever sends free text, so its
results have the autocomplete mapper's shape
(`BookInfoProxy.cs:3757-3828`):

- `gr:`-prefixed foreign ids for the book, author, and edition.
- **Every result carries `mediaType: "audiobook"`** — the mapper never assigns
  `MediaType`, and the C# default is `Audiobook = 0`
  (`src/NzbDrone.Core/Books/Model/Book.cs:100`, enum at `:16`).
- One edition per result, with **only** `foreignEditionId`, `title`,
  `titleSlug`, `overview`, `monitored: true`, `manualAdd`, `pageCount`,
  `ratings`, and `images` assigned (`BookInfoProxy.cs:3794-3822`). There is
  **no `isbn13`, `asin`, `format`, `language`, `releaseDate`, or
  `readingFormatId`**, and `isEbook` is the always-written default `false`.
  Free-text edition data can therefore never prove a format and is never
  carried into a write.
- No top-level `id` key (see serializer contract); `localEbookBooks` and
  `localAudiobookBooks` (`BookLookupController.cs:223-230`) are the bridge to
  local rows. A positive id in the requested format's array is the only local
  shortcut; never use the other format's array.

### The `mediaType` landmine

`BookLookupController.cs:86-89` filters remote results by a requested
`mediaType`. Combined with the audiobook struct default above,
**`term=<free text>&mediaType=ebook` always returns `[]`.** The bot never
sends `mediaType` on lookup, and it treats a row's `mediaType` as a
duplicate-projection preference, never a search-stage exclusion: a row
labelled `audiobook` can still be the correct work for an ebook request.

### Covers are proxied — including `remoteCover`

Every `images[].url` in a lookup response has been rewritten in place to a
relative proxied path (`MediaCoverService.cs:405-414` registers
`/MediaCoverProxy/<hash>/<file>`; stored entities get `/MediaCover/Books/...`
at `:472,480-481`). `BookResource.Images` shares the `MediaCover` object
references with the monitored edition's images (`BookResource.cs:229-231` is a
filter, not a copy), so when `remoteCover` is assigned from that same object
(`BookLookupController.cs:246-250`) it is the **same relative URL**, not the
upstream absolute one. The free-text mapper always creates its edition
monitored (`BookInfoProxy.cs:3801`), so this is the normal case. The absolute
upstream URL survives only on `images[].remoteUrl`, which is where the client
reads it. Stored-book rows always serialize `remoteCover: ""`
(`BookResource.cs:235`).

### Identity drift is structural

Canonical provider-id prefixes are `hc, gr, ol, gb, az`
(`src/NzbDrone.Core/MetadataSource/ProviderIdHelper.cs:8-20`), preferred in
that order when a primary id is chosen (`BookInfoProxy.cs:5333`,
`ProviderAmbiguityResource.cs:236-242`). Free-text results carry `gr:` ids;
imported entities normalize toward `hc:` over time, and primary ids are
documented as mutable (`docs/API_IDENTITY_AND_LIFECYCLE.md:13-46`).

`BookResource` also serializes per-provider identity sidecars — all strings,
omitted when unknown (`BookResource.cs:36-42,199-205`): `hardcoverBookId`,
`goodreadsBookId` (edition-derived, `BookEditionIdentity.cs:127-139`),
`goodreadsWorkId`, `openLibraryWorkId`, `googleBooksId`, and `asin` /
`audibleASIN` (bare uppercase ASINs with no prefix,
`BookEditionIdentity.cs:533-541`). Free-text lookup rows set only
`goodreadsWorkId` plus the edition-level `goodreadsBookId`
(`BookInfoProxy.cs:3768,3778,3796-3797`). Two hazards shape how these can be
used: a refresh copies provider ids upstream-authoritatively, so a metadata
blob missing `goodreadsWorkId` nulls the local copy (`Book.cs:286-288` with
`CleanProviderIdForCopy` `:318-331`), and `AuthorResource` carries **no**
per-provider sidecars at all — only the computed, equally drift-prone
`foreignAuthorId` (`AuthorResource.cs:27,195-222`).

The bot therefore matches local book rows through a tiered identity chain,
run only inside format-matched rows: exact `foreignBookId` equality, then
`goodreadsWorkId`, then `goodreadsBookId` (both canonical `gr:` ids,
case-insensitive), then bare-ASIN equality across `asin`/`audibleASIN`
(normalized, never parsed as `prefix:value`). A field absent on either side
skips its tier; a full miss fails closed, and the title-tier fallback applies
only when the selection carries no identity at all. Author resolution stays
exact-`foreignAuthorId` first with the single-match name fallback — with no
sidecar fields there is nothing safer to chain on. The sprint-3 canary keeps
its explicit drift probe: a `gr:` lookup must resolve a row whose primary id
normalized to `hc:`.

## Profiles

`profileType` is serialized asymmetrically, straight from the resource
declarations:

- **Quality profiles: camelCase string.** `QualityProfileResource.ProfileType`
  is the enum (`src/Chaptarr.Api.V1/Profiles/Quality/QualityProfileResource.cs:15`),
  and `ProfileType { Audiobook = 1, Ebook = 2 }`
  (`src/NzbDrone.Core/Profiles/Qualities/QualityProfile.cs:9-13`) has no
  none/general member — so the wire value is `"audiobook"` or `"ebook"`.
- **Metadata profiles: number.** `MetadataProfileResource.ProfileType` is
  declared `int` with an explicit cast
  (`src/Chaptarr.Api.V1/Profiles/Metadata/MetadataProfileResource.cs:14,86`),
  and its enum is a different one with `General = 0, Audiobook = 1, Ebook = 2`
  (`src/NzbDrone.Core/Profiles/Metadata/MetadataProfile.cs:6-11`).

Resolve configured profile names exactly at startup; a missing or ambiguous
name fails configuration validation. A missing quality profile for the
requested media type independently empties that format's searches server-side
(`ReleaseSearchService.cs:106-113`), so profile resolution is a correctness
gate, not cosmetics.

## Root folders

On 0.9.936 a root folder's nested `ebook`/`audiobook` keys are settings
objects that are present **only when the root is configured for that format**
(`src/Chaptarr.Api.V1/RootFolders/RootFolderResource.cs:46-47`; the mapper
returns null — omitted — for an unconfigured format at `:399-400`).
`folderType` is a plain int (0=Mixed, 1=Audiobook, 2=Ebook; `:39`), and flat
per-format mirror fields sit alongside the nested objects (`:50-69`):
`{ebook,audiobook}QualityProfileId`, `{ebook,audiobook}MetadataProfileId`,
`{ebook,audiobook}MonitorExisting`/`MonitorFuture` (nullable, omitted when
unconfigured), plus sidecar bools and tag lists that always serialize.

Resolution behavior is deliberately unchanged this sprint: exact configured
path or name first; otherwise explicit boolean flags/effective defaults, then
conservative name/path inference. Object presence is a valid format
discriminator per the source above and is scheduled to be consumed by the
sprint-2 root-folder rework. A root with `accessible: false` is never
selectable. Do not expose a local root path in a Discord message or public
issue report.

## Book rows

- `id` and `authorId` identify the local row and parent author; `mediaType` is
  `"ebook"` or `"audiobook"` per row — a work requested in both formats is two
  separate rows (media-typed columns throughout `Book.cs`).
- **Top-level `Book.Monitored` is a dead legacy column.** The model says so
  itself (`Book.cs:104-107`: kept for database compatibility, always false for
  new books, "Use AudiobookMonitored/EbookMonitored instead"). This dead
  column is the origin of our old misreading of monitor state. The live flags
  are `ebookMonitored`/`audiobookMonitored`, and a row-level flag must always
  be bound to its row's `mediaType`.
- On update, `PUT` bodies project **top-level `monitored`** onto the stored
  row's media-typed column (`BookResource.cs:983-991`); the per-format body
  flags are never read on update. See "Monitoring writes."
- The cross-format mutual-exclusion invariant clears the opposite format's
  flag when a format is set — unconditionally in `SetMonitored`
  (`Book.cs:415-427`), conditionally on the row's `MediaType` in the two
  `SetMonitoredForMediaType` overloads (`Book.cs:381-406,438-456`).
- `grabbed` never appears on REST (serializer contract above), so in-flight
  detection is: files present, an active exact `BookSearch` command, or the
  bot's own in-process acknowledgement cache.
- `statistics` (and per-format statistics where present), `ratings`, and
  `releaseDate` are ranking/status inputs only.

For the requested format, status is evaluated in this order:

1. **Available:** matching row has `hasFiles: true` or matching statistics
   report a positive `bookFileCount`.
2. **Active or recently acknowledged request:** an exact `BookSearch` is
   queued/started for a matching row id, or the same bot process retains a
   recent valid acknowledgement for that row.
3. **Partial request:** monitor/edition writes exist without the full verified
   state or a confirmed search. A retry repairs this state.
4. **Unmonitored:** none of the above.

## Sparse metadata is not a placeholder

0.9.936 has **no placeholder mechanism**: no `default-*` foreign ids are
minted anywhere (the only `default-` string in the repo is a CSP header), and
`Edition.IsFallbackEdition` (`Edition.cs:89`) is never set true in production
— the schema defaults it false and code only propagates it. Blank-title
editions are rejected at insert (`src/NzbDrone.Core/Books/Services/EditionService.cs:235-243`).

A row with no `releaseDate`, `images`, or `foreignEditionId` key is therefore
ordinary sparse upstream metadata, requestable like any other identity-matched
row. The former completeness gate and its guarded `RefreshAuthor` repair path
are deleted: refresh has real deletion side effects — metadata-profile
filtering during refresh prunes unprotected local rows
(`RefreshAuthorService.cs:568-623` excludes filtered books;
`RefreshEntityServiceBase.cs:136-143` deletes locals with no remote) — so a
repair path that existed only to resolve nonexistent placeholders was pure
risk. Books protected by the edition pin are un-prunable
(`RefreshBookService.cs:428-447`).

## Editions

`GET /edition?bookId=...` (`EditionController.cs`, repeatable `bookId` param)
is the only edition read for stored books.

- **`format` is verbatim provider text** ("Kindle Edition", "Hardcover",
  "Audible Audio"; `Edition.cs:41`) — display and logging only, never a
  selection input.
- **`readingFormatId` is the structured discriminator**: 1=physical, 2=audio,
  3=ebook (`Edition.cs:58`, comment inline; `EditionResource.cs:48`, `int?` —
  omitted when unset). Selection maps 2→audiobook, 3→ebook; 1 and any
  unrecognized value fail closed; 0 is the C# unset default and is treated as
  absent.
- When `readingFormatId` is absent, `isEbook: true` still proves an ebook.
  `isEbook: false` proves nothing — the edition can be physical — so it fails
  closed for writes and read-back. Read-only ranking and cover discovery may
  stay tolerant of undiscriminated projections.
- The edition pin: an edition with `ManualAdd`, or `Monitored` while the book
  has `AnyEditionOk = false`, is protected from automation re-picks
  (`src/NzbDrone.Core/Books/EditionPinPolicy.cs:20`), and such books are
  un-prunable during refresh (`RefreshBookService.cs:441-444`).

## Monitoring writes

### Two PUT routes, different pin semantics

`BookController.cs` registers two update routes: `PUT /book` (body id,
`pinExplicitEditionChange: false`; `:1791,1794`) and `PUT /book/{id}`
(`:1797,1800`, `pinExplicitEditionChange: true`). Only the `{id}` route forces
`AnyEditionOk = false` on an explicit single-monitored-edition change
(`:1827-1830`), which is what makes the pin survive refresh. The bot always
uses `PUT /book/{id}` with the complete book body, `anyEditionOk: false`, and
the full editions array carrying exactly one edition `monitored: true` +
`manualAdd: true` — the officially supported, refresh-surviving pin,
mirroring the UI's manual pick.

### What `PUT /book/{id}` actually persists

`ToModel` on update (`BookResource.cs:983-991`) reads **top-level
`monitored`** and projects it onto the stored row's media-typed column; the
per-format body flags (`ebookMonitored`/`audiobookMonitored`) are ignored on
update, and top-level `Book.Monitored` itself is the dead column described
above. Practically: edition selection persists through `PUT /book/{id}`;
book-level monitoring is written through `PUT /book/monitor`
(`{bookIds, monitored}`, `BookController.cs:1948-1955`,
`BooksMonitoredResource.cs:7-8`; returns 202 with the mapped book list), which
enforces the cross-format invariant. Both writes are verified by re-reading
`/book/{id}` and `/edition?bookId=...` before any search is queued.

### The author monitor model

`AuthorResource.cs:57-61` exposes five nullable gates, omitted when
unconfigured:

- `{ebook,audiobook}MonitorFuture` (`bool?`) — the per-format future-monitor
  gates the bot sets and verifies.
- `{ebook,audiobook}MonitorExisting` (`int?`, tri-state: 0=None, 1=All,
  2=Selected, null=unconfigured).
- `syncMonitoredAcrossFormats` (`bool?`).

The author gate is ANDed into search eligibility at the SQL level
(`AuthorExtensions.cs:16-93`), so an unset gate silently empties searches even
for a monitored book — which is why the bot enables and read-back-verifies the
requested format's `*MonitorFuture` before searching.

`PUT /author/{id}/monitor/{mediaType}` is a landmine, not a shortcut: it
never touches the author entity. It bulk-rewrites the per-book media-type
monitor column for **every** book of the author via raw SQL
(`AuthorController.cs:1154-1166` → `AuthorService.cs:1147-1153` →
`BookRepository.cs:679-716`, unfiltered by media type), returns a bare 200,
and leaves `*MonitorFuture`/`*MonitorExisting` unchanged — automatic search
stays gated off (`AuthorExtensions.cs:16-39`) while unrelated books' monitor
state is clobbered. The author gate remains the full-resource
`PUT /author/{id}` setting the format's `*MonitorFuture` plus `monitored`,
verified by re-read. And never send `booksToMonitor` in any body: a
`SpecificBook` monitor type with an empty list throws server-side
(`BookMonitoredService.cs:113-114`).

## Duplicate pockets are intentional data model

A work exists per media type as separate Book rows (media-typed columns and
per-row `mediaType` throughout `Book.cs`); only bit-identical pockets
coalesce. Imports can additionally leave duplicate local rows for one
`foreignBookId` within a format (logged server-side as
`[SERVER-BUG-CANDIDATE] ... provider id(s) appearing in multiple pockets` —
an operational observation from the 2026-07-15 incident). The pockets are not
equivalent: one row can carry usable requested-format editions while its twin
carries none. Requests choose the row with usable requested-format editions
and monitor only that row; already-requested checks span every matching row.

## Catalog settling

The post-add monitor-rewrite hazard is one shot. `AuthorScannedHandler` gates
on `AddOptions != null` (`AuthorScannedHandler.cs:41`), bulk-rewrites the
author's book monitor flags per `AddOptions` (`:43`), re-persists the author
(`BookMonitoredService.cs:118-125`), and only then clears `AddOptions`
(`:50-51`). It handles both `AuthorScannedEvent` and `AuthorScanSkippedEvent`
(`AuthorScannedHandler.cs:11-12,58-66`), so the latch clears on the scan path
(`RefreshAuthor` → `RescanFolders` → `DiskScanService.cs:892-896`) and on the
no-folder-evidence skip path (`RefreshAuthorService.cs:2170-2178`, where no
`RescanFolders` is ever pushed). Nothing else clears `AddOptions` — the
handler is `RemoveAddOptions`'s only production call site, and a full-resource
`PUT /author/{id}` cannot overwrite it (`AuthorResource.cs:571-573`,
`AuthorService.cs:634-636`). `GET /author/{id}` serializes `addOptions`
(`AuthorResource.cs:81,159`) with nulls omitted, so a missing key means the
handler already ran.

The bot's settle gate is therefore a composite latch, not a sampling loop:
`addOptions` absent on `GET /author/{id}` AND no queued/started
author-relevant command, inside a hard deadline; poll errors and the deadline
fail closed, and a long-settled author passes on the first look. No
book/edition fingerprint sampling is needed once the latch is spent, because
refresh preserves existing rows' monitor flags (`Book.cs:337-338`, plus
`AnyEditionOk` `:339` and `AddOptions` `:343`), new rows are monitored only
under `MonitorExisting == 1` (`RefreshAuthorService.cs:1804-1805`), and the
manual edition pin is protected (`EditionPinPolicy.cs:20`; pinned books
un-prunable, `RefreshBookService.cs:428-447`). The author-gate write stays
after settle because the handler's rewrite re-persists the author. Dead-end
signals, for the record: `book.addOptions` is never null (constructor
default, `Book.cs:30`) and `AuthorScanCompletedEvent` has zero consumers.
(That a 400+ book author imports for minutes is an operational observation.)

## POST /book hard edges

`BookController.cs:1175-1384`:

- `mediaType` comes from query or body (`:1198`); when neither is supplied, a
  Seerr-compat path adds both formats (`:1206-1312`). The bot always sends an
  explicit `mediaType`.
- **400 unless an upstream provider work id survives mapping** —
  `IsMissingUpstreamProviderBookId` (`:1386-1397`; driving 400s at
  `:1253,1285,1348`).
- Bare (unprefixed) foreign ids are rejected on the native API
  (`:1399-1430`, a `ValidationException` when the id has no `:`; the facade
  path is exempt).
- Legacy singular `qualityProfileId`/`metadataProfileId` are translated by
  profile type when per-format fields are absent (`:1613-1663`); our four
  per-format ids remain the better payload. Per-format profiles live on the
  nested `author`, not the book resource.
- Responses: `201 Created` with a full `BookResource` (via
  `RestController.cs:149-153`); **`202 Accepted` with
  `PendingBookRequestResource {pendingId, message}`**
  (`PendingBookRequestResource.cs:5-6`) when upstream metadata is unavailable;
  and a **409** with `ProviderAmbiguityResource`
  (`ProviderAmbiguityResource.cs:41` pins the status code; properties `error`,
  `message`, `entityType`, `field`, `providerId`, `mediaType`, `candidates`)
  on ambiguous provider identity. The bot branches on all three: a 202 stops
  with "Chaptarr queued this work upstream - try again in a few minutes", a
  409 stops with a message naming the conflicting candidates, and a 201 is
  still only an acknowledgement whose identity the bot re-resolves itself.
  These meanings are specific to `POST /book` (`PUT /book/monitor` also
  answers 202, as ordinary success).

A post response is only an acknowledgement and never implies that a usable or
correctly identified row already exists.

## Search and selection invariants

Search should be useful without becoming inventive:

1. Normalize whitespace, case, and punctuation for comparison while retaining
   the original title for display.
2. Display the author beside the title so common titles are distinguishable.
3. Drop obvious non-work results such as study guides, SparkNotes/CliffsNotes,
   summaries and analysis, unofficial companions, lesson plans, and
   conversation starters. Use conservative multi-word markers; do not reject a
   legitimate title merely because it contains `guide` or `summary`. The
   lookup path has zero server-side filtering, so this stays client-side.
4. Prefer exact normalized title matches, then narrowly allow subtitle
   variants separated by `:`, `-`, `—`, or parentheses. A plain shared prefix
   is not a match. Never cross authors after the user has selected one.
5. This provider requests one work per Discord interaction. Reject results
   with clear multi-book title signals — a title ending in `bundle` or
   `trilogy`, an `omnibus`, a box set, a `complete ... series`, or an explicit
   numbered book collection/set — with an instruction to request an individual
   title. A bare word such as `collection` or `series` is not sufficient. Do
   not expand one selection into multiple works.
6. Within a title tier, prefer a requested-format `mediaType` projection, a
   resolved row, then popularity, votes, and release date as tie-breakers.
7. If no row matches the selected title and format, stop. Falling back to a
   popular sibling title requests the wrong book.

## Cover selection

Cover rendering is strictly read-only; the bot never writes merely to obtain a
cover. Per the proxied-cover truth above, the order is:

1. A fully qualified HTTPS URL among the lookup images — in practice
   `images[].remoteUrl`, since `images[].url` and `remoteCover` are relative
   proxied paths on 0.9.936.
2. A compatible edition image's HTTPS URL (same `remoteUrl` rule).
3. An Open Library ISBN cover when a compatible edition carries a valid
   ISBN-13 — provider-id path lookups only, since free-text editions carry no
   ISBN (`BookInfoProxy.cs:3794-3810`).
4. A cover from one best-effort, rate-limited Open Library Search call per
   Chaptarr search, accepted only on a normalized title-and-author match
   ([Search API][openlibrary-search], [Covers API][openlibrary-covers];
   `default=false` returns 404 instead of a placeholder image).
5. No cover. A cover failure never blocks a request.

Relative Chaptarr cover URLs are never exposed to Discord: they resolve
against the (often private) Chaptarr host. Open Library enrichment defaults to
enabled and may be disabled per backend with `openlibrary_covers = false`;
when enabled, the search text leaves the local network.

## Safe request sequence

The sequence is an invariant. Keeping confirmation read-only and verifying
every silent-write-prone step is more important than minimizing GETs.

1. At startup, fetch status, profiles, and roots. Validate version and all
   configured names/paths before accepting a command.
2. Search with `GET /book/lookup` (never sending `mediaType`). Filter and rank.
   One bounded, best-effort Open Library call may enrich covers; no writes.
3. Let the user select a result and confirm. Disable the Request button
   immediately after the click.
4. Re-resolve the selected identity under the per-work mutation lock.
   Short-circuit an available work or a fully consistent active request. A
   bare monitor flag is not proof a search was queued; partial state is
   carried forward for repair.
5. If the author is new, `POST /book` with both roots, all four per-format
   profile ids, an explicit `mediaType`, every book-level monitor flag false,
   only the requested format's `*MonitorFuture` gate true, and search-on-add
   false. If the author exists but the work does not, post the selected work
   with the local `authorId` and a neutral requested-format edition
   placeholder (free-text lookup editions are never carried into writes).
6. After any add, wait for the catalog to settle (see "Catalog settling").
7. Resolve the target row: identity match (exact `foreignBookId`, title tier
   only when the selection has no work id), format-bound, multi-book titles
   rejected. Sparse metadata does not block resolution.
8. Re-resolve edition-aware: fetch `/edition?bookId=...` for every matching
   row, disambiguate pockets by usable requested-format editions, choose one
   edition via `readingFormatId` (English preferred, junk demoted, projected
   edition honored). No usable edition stops with an actionable error and a
   log of what the server offered.
9. Read the author; enable and re-verify the requested format's
   `*MonitorFuture` gate if needed.
10. Select the edition with `PUT /book/{id}` (complete body,
    `anyEditionOk: false`, exactly one `monitored: true, manualAdd: true`
    edition).
11. `PUT /book/monitor` with `{"bookIds":[id],"monitored":true}`.
12. Re-read `/book/{id}` and `/edition?bookId=...`; require the row still
    matches, the row is monitored for the requested format, and exactly one
    requested-format edition — the chosen one — is monitored. Any miss stops
    before search.
13. `POST /command` `{"name":"BookSearch","bookIds":[id]}` and require a valid
    acknowledgement before reporting success.
14. Retries are convergent with bounded deduplication: stop for an available
    file, an exact queued/started `BookSearch`, or a recent in-process
    acknowledgement; otherwise repair partial state through steps 8-13. Never
    create duplicate authors or queue a second concurrent search.

Every write-path change remains beta until the exact candidate image passes a
disposable-library live canary covering new-author settle, multi-book
rejection, edition selection, strict read-back, partial-state retry, and
`BookSearch` acknowledgement. Fixtures cannot prove that a mutation persists.

## Fixture provenance

Fixtures live in `doplarr/tests/fixtures/chaptarr/`. All titles, names, ids,
paths, dates, and URLs are synthetic. No user, library, API key, or private
hostname appears in them. Every shape follows the serializer contract above;
each row lists the source mechanism it models.

| Fixture | Shape modeled (source) |
| --- | --- |
| `system_status.json` | Status discriminator at the tested 0.9.936 baseline |
| `lookup.json` | Free-text autocomplete projections: `gr:` ids, audiobook `mediaType` default, discriminator-free monitored editions, proxied `url`/`remoteCover` with absolute `remoteUrl`, local bridge arrays, junk row for the client-side filter (`BookInfoProxy.cs:3757-3828`, `BookLookupController.cs:223-250`) |
| `lookup_audiobook_projection.json` | Same free-text shape, single result, no local bridges |
| `openlibrary_search.json` | Official Search API `cover_i` shape with exact and non-matching rows |
| `author.json` | Local author row: normalized `hc:` identity, per-format gates and profile ids |
| `book_available.json` | `/book` row with files, no `editions`/`grabbed` keys, `remoteCover: ""` (`BookResource.cs:142-259`) |
| `book_processing.json` | Monitored, file-less audiobook row (in-flight via monitor flags, never `grabbed`) |
| `book_unmonitored.json` | Resolved row eligible for the monitor sequence |
| `book_sparse.json` | Sparse upstream metadata: no `releaseDate`/`images`/`foreignEditionId` keys — requestable, not a placeholder (`Edition.cs:89` dead fallback flag) |
| `edition_formats.json` | `/edition` rows: provider-text `format`, nullable `readingFormatId` 1/2/3 plus a legacy row without it (`Edition.cs:41,58`, `EditionResource.cs`) |
| `quality_profiles.json` | String `profileType` (`QualityProfileResource.cs:15`) |
| `metadata_profiles.json` | Numeric `profileType` with `General = 0` (`MetadataProfileResource.cs:14,86`) |
| `root_folders.json` | Nested settings present only when configured, `folderType` ints, flat mirrors (`RootFolderResource.cs:39-69,399-400`) |
| `root_folders_nested.json` | The same contract as captured live: one configured format per root |
| `post_book_response.json` | 201-path created row without null keys or minted placeholder ids |
| `post_book_pending.json` | 202-path `PendingBookRequestResource {pendingId, message}` (`PendingBookRequestResource.cs:5-6`) |
| `put_monitor_response.json` | Non-authoritative acknowledgement snippet; never parsed for verification |
| `command_response.json` | Servarr-style queued-command acknowledgement; only acknowledgement is relevant |

The mutation-response fixtures are intentionally non-authoritative. The
implementation deserializes them tolerantly, then establishes truth through
the read-back steps above.

## Drift policy

Chaptarr is pre-1.0, so a patch-looking release can still change an API shape.

- Parse only fields this contract uses and tolerate unknown fields.
- Require `appName: "Chaptarr"` and a non-empty live version at startup.
  `0.9.936.x` is the tested baseline; a different non-empty version receives a
  clear untested-version warning.
- Run the exact candidate image with `--check /config.toml` before Discord
  startup. The command exercises status, root, quality-profile, and
  metadata-profile parsing, emits only a sanitized summary, and exits without
  constructing a Discord client. A version outside the tested line produces an
  explicit `unsupported` report and nonzero exit.
- If required identity or format fields are absent, disable Chaptarr writes
  for that interaction and return a useful compatibility error. Do not guess.
- Before claiming support for a new release, re-derive this contract's cited
  mechanisms against the new source ref, update fixtures to the serializer
  truth, and run the contract tests. Handling for mid-line upstream drift is
  sprint-3 scope.
- A write-path change also requires the exact candidate image to pass the
  disposable live canary in `RELEASE_CHECKLIST.md`. Read-only evidence cannot
  prove a mutation still persists.
- **Never codegen from `openapi.json`.** The spec is committed
  (`src/Chaptarr.Api.V1/openapi.json`) but mistypes `CommandResource.body` and
  parameter optionality, so generated models would encode wrong contracts with
  false confidence. It is useful as a route inventory only. The handwritten
  narrow client — and this document — remain the contract.

Rust Doplarr's own developer guide supports adding a backend through the
`MediaBackend` and `MediaItem` traits. Keeping Chaptarr behind that provider
boundary keeps this compatibility layer replaceable; see
[Doplarr developer documentation][doplarr-dev].

[openlibrary-search]: https://openlibrary.org/dev/docs/api/search
[openlibrary-covers]: https://openlibrary.org/dev/docs/api/covers
[doplarr-dev]: https://github.com/activexray/doplarr_rs/blob/main/README_DEVELOPER.md
