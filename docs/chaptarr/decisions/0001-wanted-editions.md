# 0001 — `POST /book/{id}/editions/wanted` is not adopted

**Status:** decided (Sprint 2, 2026-08-27)
**Verdict:** NOT adopted for the request pipeline
**Source:** Chaptarr v0.9.936 (`develop @ 423b1bb`), citations line-verified

## Question

Could the request choreography (settle → author gate → edition pin →
`PUT /book/monitor` → read-back verify → `POST /command BookSearch`) collapse
onto the server's own wanted-edition endpoint?

## Verified facts

- Route `POST /api/v1/book/{id}/editions/wanted`
  (`BookController.cs:1957-1989`), body
  `{editionId: int, searchForNewBook: bool = false}`
  (`AddWantedEditionRequest.cs:3-10`), handled by
  `BookService.AddWantedEdition` (`BookService.cs:2094-2392`).
- **Audiobook-only.** An ebook book throws
  `InvalidOperationException("Wanted narrator editions can only be created
  for audiobook books")` (`:2102-2105`). Half the pipeline could never use
  it.
- **Requires an already-local book and edition** (`:2096-2116`); it cannot
  add a work.
- **Never opens the author format gate.** Its only author interaction is a
  read (`:2250`); no `UpdateAuthor`, no profile writes. Automatic-search
  eligibility stays gated on `MonitorExisting > 0 || MonitorFuture == true`
  (`AuthorExtensions.cs:16-39`).
- **Its search silently no-ops while that gate is closed.** The
  `BookSearchCommand` it queues carries a non-manual trigger, and non-manual
  searches are filtered through `IsMonitoredWithAuthor()`
  (`BookSearchService.cs:168-181`). An explicit `POST /command` is stamped
  `Trigger = Manual` (`CommandController.cs:114`) and bypasses that filter —
  our existing search is strictly stronger.
- **Edition re-pin failure is swallowed.** The create branch's
  `SetMonitored(..., isManualSelection: true)` (`:2383`) runs in a `try`
  whose `catch` only logs a warning (`:2386-2389`); the caller still sees
  success.
- **It may return a row other than the one acted on:** the no-files branch
  pins in place (`:2119-2146`), the dedupe branch can return an existing
  sibling (`:2148-2246`), and the create branch mints a `_wanted_` row with
  `AddType = Manual` (`:2265`, `:2290`).

## Decision

Not adopted. Every property the pipeline needs — both formats, add-capable,
gate-opening, verified pin, manual-trigger search — is absent or strictly
weaker here. The explicit choreography stays, and no pipeline variant is
built on this endpoint.

## Future consideration

An audiobook narrator-selection feature could revisit it: this is the
server's native "want this narrator's edition" operation, and narrator
fields become visible on a row once files exist or an edition is pinned
(`BookResource.cs:249-256`). Any revisit requires fresh source verification
at that release.

## Canary

`RELEASE_CHECKLIST.md` carries a one-time probe against the disposable
instance to confirm live behavior matches this reading.
