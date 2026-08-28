# Two dead fields on the book/edition models: `Book.Monitored` and `Edition.IsFallbackEdition`

Verified against v0.9.936 (develop @ 423b1bb).

## Summary

Thanks for Chaptarr. We build a Discord request bot against the v1 API, and we read the
entity models to work out what the resources mean. Two fields cost us time because they
look like live state and are not. Neither is a bug that breaks a request today — both are
more like traps for the next reader, and one of them has a doc comment that is no longer
accurate. We wanted to flag them while they were fresh.

To be precise about scope: neither field is serialized on the v1 resources, so no API
response currently carries a wrong value because of this. `BookResource.Monitored` is
derived from the per-format flags and is correct.

## Observed behavior

**1. `Book.Monitored` has no database column, is ignored by the ORM, and is still assigned.**

The property carries a comment saying it is "kept for database compatibility but not used"
and is "always set to false for new books". Both halves are stale. The `Books` table has no
`Monitored` column at all — the schema migration says so in a comment, and the index builder
has a matching "Removed: Books does not have a single Monitored column" note. The table
mapping explicitly calls `.Ignore(x => x.Monitored)`. Real monitoring state lives in
`AudiobookMonitored` / `EbookMonitored`, reached through `IsMonitored()` /
`SetMonitored()` / `SetMonitoredForMediaType()`.

Despite that, five production call sites assign `true` to it, some immediately after
calling `SetMonitored(true)` and one with a comment about "keeping the legacy Monitored
field consistent". Those assignments are no-ops that never reach the database, and the
in-memory value they leave behind can disagree with the per-format flags for the rest of
the request.

**2. `Edition.IsFallbackEdition` is never computed and can only ever hold its column default.**

The column exists with a default of `false`. The property is copied forward in three
places, but every one of them copies from another edition — there is no site in production
code that ever computes or sets it to `true`. The only assignment of `true` anywhere in the
tree is a test fixture. It is nonetheless read as a behavioral branch in the match bench
tool, so that branch is unreachable in practice.

## Source citations (file:line, verified)

`Book.Monitored`:
- `src/NzbDrone.Core/Books/Model/Book.cs:104-107` — the stale comment and the property
- `src/NzbDrone.Core/Books/Model/Book.cs:366-424` — the live per-format accessors
- `src/NzbDrone.Core/Datastore/Migration/001_chaptarr_complete_schema.cs:283-294` — `Books` table: `AudiobookMonitored` / `EbookMonitored`, no `Monitored`
- `src/NzbDrone.Core/Datastore/Migration/001_chaptarr_complete_schema.cs:1126` — "Removed: Books does not have a single Monitored column"
- `src/NzbDrone.Core/Datastore/TableMapping.cs:168-169` — `.Ignore(x => x.Monitored)`
- Assignments that go nowhere: `src/NzbDrone.Core/Books/Services/AddBookService.cs:262`, `:291`, `:445`; `src/NzbDrone.Core/Books/Services/BookService.cs:2141`; `src/NzbDrone.Core/Books/Handlers/MonitorOnFileAddedHandler.cs:91`, `:106`
- Correct API mapping (for contrast): `src/Chaptarr.Api.V1/Books/BookResource.cs:208` — `Monitored = model.IsMonitored()`

`Edition.IsFallbackEdition`:
- `src/NzbDrone.Core/Books/Model/Edition.cs:89` — the property
- `src/NzbDrone.Core/Datastore/Migration/001_chaptarr_complete_schema.cs:379` — column, default `false`
- Copy-forward only: `src/NzbDrone.Core/Books/Model/Edition.cs:248`; `src/NzbDrone.Core/Books/Services/RefreshEntityCopy.cs:108`; `src/NzbDrone.Core/ImportLists/ImportListSyncService.cs:1070`
- Only `true` assignment in the tree: `src/Chaptarr.Core.Test/Books/EditionUseMetadataFromConvergenceFixture.cs:22`
- Read as a branch: `src/NzbDrone.MatchBench/Program.cs:3258`

## Why it matters to API clients

Not through the wire format — we want to be clear that we did not observe a wrong value in
any response. The cost is to anyone modelling the API from the source, which is what we did
because the served spec did not describe the request shapes we needed. `Book.Monitored`
reads as the monitoring flag and is the one that never persists, and the comment on it
points at a database column that does not exist, so the reader has to go to the migration
and the table mapping to find out. `IsFallbackEdition` reads as a meaningful edition
property and is always `false`.

There is also a smaller correctness concern we cannot evaluate from outside: the five
`book.Monitored = true` assignments sit next to real per-format writes, which suggests
someone believed they were doing something.

## Suggested fix

- Delete `Book.Monitored` along with the five assignments, or, if it is kept as a shim,
  correct the comment to say there is no backing column and that the ORM ignores it.
- For `IsFallbackEdition`, either wire up whatever was meant to set it, or drop the
  property and the match bench branch that keys on it. If the column has to stay for
  existing databases, a comment saying it is always the default would be enough.
