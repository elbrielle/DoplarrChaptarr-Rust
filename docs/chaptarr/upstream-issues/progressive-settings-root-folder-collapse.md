# Existing-author progressive settings collapse both root folders into one parameter, preferring audiobook

Verified against v0.9.936 (develop @ 423b1bb).

## Summary

Thanks for Chaptarr. We build a Discord request bot against the v1 API. When
`POST /api/v1/book` adds a work under an existing author through the catalog
path, `ApplyExistingAuthorProgressiveSettings` hands
`UpdateAuthorProgressiveSettings` a single root-folder path shared by both
formats, computed as `config.AudiobookRootFolderPath ??
config.EbookRootFolderPath`. The progressive fill then writes that one value
into whichever format it is configuring. An ebook add whose nested `author`
carries both formats' root paths therefore persists the **audiobook** root
as the author's `EbookRootFolderPath`, and later ebook imports for that
author organize into the audiobook library.

The inline comment acknowledges the collapse ("Single path param: prefer
audiobook path if present else ebook"). The dedupe branch in
`AddBookService` does not have the defect — it calls the same method once
per format and passes each format's own root.

## Reproduction sketch

1. An author exists configured for audiobooks only (created by an
   audiobook-only add — the intended lazy per-format initialization).
2. `POST /api/v1/book?mediaType=ebook` for a work with no local row, the
   nested `author` carrying `ebookQualityProfileId`, `ebookRootFolderPath`,
   and — as a client reusing one author payload naturally does — the
   audiobook fields as well.
3. `BuildRequestedBookMonitoringConfig` copies both roots into the config;
   `CreateEbook = true`, `CreateAudiobook = false`. The per-format profile
   arguments for audiobook are nulled by the `CreateAudiobook` gates, but
   the shared root parameter is not gated and resolves to the audiobook
   path.
4. Inside `UpdateAuthorProgressiveSettings`, the ebook block runs (its
   quality profile id is present), finds `EbookRootFolderPath` unset, and
   fills it with the shared parameter — the audiobook root.

## Source citations (file:line, verified)

- `src/NzbDrone.Core/Books/Services/AuthorLibraryService.cs:1126-1137` —
  the call site; line 1137 collapses both roots with the
  audiobook-preferring `??` under the "Single path param" comment.
- `src/NzbDrone.Core/Books/Services/AuthorService.cs:677-765` —
  `UpdateAuthorProgressiveSettings`; the ebook block writes the shared
  `rootFolderPath` parameter into `EbookRootFolderPath` at `:751-757`
  (audiobook mirror at `:714-719`).
- `src/NzbDrone.Core/Books/Services/AddBookService.cs:492-512` —
  `BuildRequestedBookMonitoringConfig` copies both formats' roots into the
  config ungated, so the collapse point sees both.
- `src/NzbDrone.Core/Books/Services/AddBookService.cs:186-224` — the
  dedupe branch calling the same method with each format's own root
  (`:205`, `:222`), showing the intended per-format shape.

## Suggested fix

Pass both roots through — either two parameters or one call per format, as
the dedupe branch already does — and fill each format only from its own
root.

## Workaround

A client can avoid the wrong-root fill by sending only the requested
format's root path in existing-author add bodies, so the `??` never has a
sibling path to prefer.
