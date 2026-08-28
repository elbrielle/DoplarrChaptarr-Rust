# `RootFolderController` injects the profile-exists validators but never attaches them

Verified against v0.9.936 (develop @ 423b1bb).

## Summary

Thanks for Chaptarr. We build a Discord request bot against the v1 API and provision root
folders through it. `RootFolderController` takes `QualityProfileExistsValidator` and
`MetadataProfileExistsValidator` as constructor parameters, but neither is stored in a
field or passed to a `SetValidator` call anywhere in the file. The result is that
`POST /api/v1/rootfolder` accepts profile ids that do not exist and returns 201.

This looks like an oversight rather than a decision, because other controllers that take
the same two validators do attach them — `AuthorController` and `ImportListController`
both do.

## Observed behavior

`POST /api/v1/rootfolder` with an `audiobookQualityProfileId`, `audiobookMetadataProfileId`,
`ebookQualityProfileId`, or `ebookMetadataProfileId` that matches no existing profile
succeeds. The root folder is created with the bad id stored on it. Nothing in the response
indicates a problem. The failure only appears later, when something tries to resolve the
profile for that root folder.

The same holds for the `QualityProfileId`/`MetadataProfileId` inside the nested
`audiobook`/`ebook` settings objects (`MediaTypeSettingsResource`), which are equally
unvalidated. (The resource has no top-level profile ids — a comment marks them as
deleted in favor of the media-specific settings.)

The controller's constructor does attach a long chain of path validators
(`mappedNetworkDriveValidator`, `startupFolderValidator`, `recycleBinValidator`,
`pathExistsValidator`, `systemFolderValidator`, `folderReadableValidator`,
`folderWritableValidator`, `rootFolderValidator`) plus name, host, port, url-base, username,
password, and Calibre output-format rules. Only the two profile validators are dropped.

## Source citations (file:line, verified)

- `src/Chaptarr.Api.V1/RootFolders/RootFolderController.cs:47-48` — the two validators arrive as constructor parameters
- `src/Chaptarr.Api.V1/RootFolders/RootFolderController.cs:51-56` — the assignment block; neither validator is assigned to a field
- `src/Chaptarr.Api.V1/RootFolders/RootFolderController.cs:58-87` — the full validator setup; no `SetValidator` call references either one. Those are the only two occurrences of the parameters in the file.
- `src/Chaptarr.Api.V1/RootFolders/RootFolderResource.cs:27` — comment marking the
  top-level profile ids as deleted in favor of media-specific settings
- `src/Chaptarr.Api.V1/RootFolders/RootFolderResource.cs:56-59` — the four per-format profile ids
- `src/Chaptarr.Api.V1/RootFolders/RootFolderResource.cs:12-20` — `MediaTypeSettingsResource` with the nested `QualityProfileId`/`MetadataProfileId` (`:14-15`)
- `src/Chaptarr.Api.V1/RootFolders/RootFolderController.cs:114-125` — `CreateRootFolder`: maps to model and adds, with no profile check of its own
- `src/NzbDrone.Core/Validation/QualityProfileExistsValidator.cs:17-25` — the validator treats null and `0` as valid, so attaching it would not break callers who omit the field

Controllers that do attach the same validators, for contrast:
- `src/Chaptarr.Api.V1/Author/AuthorController.cs:135-136` (parameters) and `:181`, `:185`, `:188`, `:191`, `:194` (attachments)
- `src/Chaptarr.Api.V1/ImportLists/ImportListController.cs:15-16` (parameters) and `:31-32` (attachments)

## Why it matters to API clients

A typo in a profile id is one of the easiest mistakes to make when provisioning over HTTP,
and it is exactly the mistake a write-time validator is there to catch. Right now the write
succeeds, so the client has no signal, and the resulting root folder misbehaves later at a
point that has no obvious connection to the request that created it. We have to re-read the
root folder and cross-check its profile ids against `/api/v1/qualityprofile` and
`/api/v1/metadataprofile` ourselves to get a usable error message for the person who made
the request.

The `AuthorController` behavior sets the expectation we coded against: post a bad profile id
to an author and you get a validation failure. Root folders behaving differently was
surprising.

## Suggested fix

Attach the injected validators over the four per-format ids, following the exact pattern
`AuthorController` already uses for the same fields (`AuthorController.cs:184-195`):

```csharp
SharedValidator.RuleFor(c => c.AudiobookQualityProfileId)
               .SetValidator(qualityProfileExistsValidator)
               .When(c => c.AudiobookQualityProfileId.HasValue && c.AudiobookQualityProfileId.Value > 0);
SharedValidator.RuleFor(c => c.EbookQualityProfileId)
               .SetValidator(qualityProfileExistsValidator)
               .When(c => c.EbookQualityProfileId.HasValue && c.EbookQualityProfileId.Value > 0);
SharedValidator.RuleFor(c => c.AudiobookMetadataProfileId)
               .SetValidator(metadataProfileExistsValidator)
               .When(c => c.AudiobookMetadataProfileId.HasValue && c.AudiobookMetadataProfileId.Value > 0);
SharedValidator.RuleFor(c => c.EbookMetadataProfileId)
               .SetValidator(metadataProfileExistsValidator)
               .When(c => c.EbookMetadataProfileId.HasValue && c.EbookMetadataProfileId.Value > 0);
```

and the same for the nested settings' ids if those should be checked too. Because the
validators pass on null and `0`, adding them should not affect callers that leave the
fields unset. If the omission was deliberate, dropping the two unused constructor
parameters would make that clear to readers.
