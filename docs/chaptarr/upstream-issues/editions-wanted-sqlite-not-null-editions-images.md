# `POST /api/v1/book` returns 409 carrying a raw SQLite NOT NULL error (`Editions.Images`)

Verified against v0.9.936 (develop @ 423b1bb). Behavior observed live on a fresh
`chaptarr/chaptarr:0.9.936` container on 2026-08-28.

## Summary

Thanks for Chaptarr. We build a Discord request bot against the v1 API. Adding the second
format of a work that is already in the library fails, and it fails in a way that is hard to
handle: the response is `409 Conflict`, but the body is not the ambiguity resource that a
409 means everywhere else on this endpoint. It is a bare error object whose message is a
raw SQLite constraint failure.

Two separate things combine here: an insert that writes `NULL` into a `NOT NULL` column,
and an error-pipeline rule that turns any SQLite constraint violation on `POST`/`PUT` into
`409`, which collides with the status the endpoint already uses for provider-identity
ambiguity.

## Observed behavior

`POST /api/v1/book` for the other format of an already-local work returns:

```
HTTP/1.1 409 Conflict
{"message":"SQLite Error 19: 'NOT NULL constraint failed: Editions.Images'.","description":null}
```

The book is not added. Repeating the request reproduces it. From a client's point of view
this is indistinguishable by status code from the documented ambiguity conflict, and the
body does not match the documented schema for that status, so shape-checking clients fail
to parse it and status-checking clients report a nonexistent identity conflict to the user.

## Source citations (file:line, verified)

The constraint:
- The observed error message is the authority that the column is NOT NULL. The migration
  declares it at `src/NzbDrone.Core/Datastore/Migration/001_chaptarr_complete_schema.cs:380` —
  `.WithColumn("Images").AsString().WithDefaultValue("[]")` — and the default only applies
  when the column is omitted from the insert, so an explicit `NULL` still violates it,
  which matches the observed message. `Links` at `:381` has the same shape.

The write path that can carry a null:
- `src/Chaptarr.Api.V1/Books/BookResource.cs:736` — `Editions = resource.Editions?.Select(e => e.ToModel(facadeContext))...`
- `src/Chaptarr.Api.V1/Books/EditionResource.cs:188` — `Images = resource.Images` with no null coalesce, so a posted edition that omits `images` produces `Edition.Images == null`. Note the neighbouring lines do guard: `Asins` at `:177`, `NarratorNames` at `:202`, `Chapters` at `:207-213` all use `?? new List<...>()`. `Links` at `:189` is unguarded in the same way.
- `src/NzbDrone.Core/Books/Model/Edition.cs:15-25` — the parameterless constructor initialises `Images` to an empty list, which is why the `new Edition { ... }` sites elsewhere are safe. `ToModel` overwrites that with the resource's value.
- `src/NzbDrone.Core/Books/Services/AddBookService.cs:137` with the predicate at
  `:940-949` — a posted editions array whose entries carry a `title` and a
  `readingFormatId` is *kept*; only "lesser" payloads (empty, null entries, blank
  titles, or no reading format) get wholesale-replaced from the database. So the null
  `Images` survives to the insert exactly when the client posts an otherwise
  well-formed edition that omits `images` — which is what our add body did.
- `src/NzbDrone.Core/Books/Services/BookService.cs:2286`, `:2326-2327` — the sibling-minting path already null-guards `Images` and `Links` explicitly, so the defensive pattern exists in the codebase.
- `src/NzbDrone.Core/Books/Services/AuthorService.cs:160-172` — the author path already
  does exactly the guard we are asking for, with a comment saying why: "Ensure embedded
  documents can't violate NOT NULL constraints or deserialize to null", followed by
  `author.Images ??= new List<MediaCover.MediaCover>();` and the same for `Links`,
  `Genres`, and the rest.

We could not confirm from source alone which specific insert fired in our live run, so
treat that final step as inferred; the constraint, the unguarded mapping at
`EditionResource.cs:188`, the payload condition at `AddBookService.cs:137`/`:940-949`,
and the observed message are all directly verified.

The status-code mapping:
- `src/Chaptarr.Http/ErrorManagement/ChaptarrErrorPipeline.cs:79-88` — on `POST`/`PUT`, any `SqliteException` with `SqliteErrorCode == 19` becomes `HttpStatusCode.Conflict`. Code 19 is `SQLITE_CONSTRAINT` generally, which covers NOT NULL, CHECK, and FOREIGN KEY as well as UNIQUE; the comment on `:83` says "Unique/constraint violation", so the intent looks like it was UNIQUE only.
- `src/Chaptarr.Http/ErrorManagement/ChaptarrErrorPipeline.cs:34-38` — the body is `ErrorModel { Message = exception.Message, Description = null }`, which is exactly the shape we received.
- `src/Chaptarr.Api.V1/ProviderIds/ProviderAmbiguityResource.cs:41` — `public const int StatusCode = 409`, the status the endpoint uses for provider ambiguity.
- `src/Chaptarr.Api.V1/Books/BookController.cs:1176` — `[ProducesResponseType(typeof(ProviderAmbiguityResource), ProviderAmbiguityHelper.StatusCode)]`
- `src/Chaptarr.Api.V1/openapi.json:1385-1404` — the served spec documents `409 → ProviderAmbiguityResource` for this operation.

## Why it matters to API clients

A server-side data-shape defect is reported to us as an identity conflict. 409 on this
endpoint has a documented meaning and a documented body, and we branch on it: on a real
ambiguity we ask the requester to pick between candidates. Here we get a 409 whose body has
no `candidates`, so the branch either throws on deserialization or shows the user a
disambiguation prompt with nothing to disambiguate. The raw SQLite text also leaks storage
internals into a client-facing message.

The practical effect is that cross-format adds are unusable for affected works. Our bot
cannot tell the requester anything more useful than that the add failed.

## Suggested fix

Two independent changes, either of which improves things on its own:

1. Null-guard the list mappings in `EditionResource.ToModel` the way the neighbouring
   fields already are: `Images = resource.Images ?? new List<MediaCover>()` and the same
   for `Links`, so an omitted `images` array cannot reach the insert as `NULL`.
2. Narrow the 409 mapping in `ChaptarrErrorPipeline` to the unique-constraint case the
   comment describes — `SqliteExtendedErrorCode == 2067` (and `1555` for primary-key
   conflicts) — and let other constraint failures fall through to 500, so a storage bug
   is not reported with a status that has a contract attached to it. Replacing the raw
   `exception.Message` with a generic message for non-`ApiException` cases would also
   keep SQL text out of responses.
