# Publishing the Rust rewrite

This checkout and the existing public DoplarrChaptarr repository do not share
Git history. The public repository's `main` branch contains the Clojure fork;
this rewrite begins from `activexray/doplarr_rs`. It must not be published as an
ordinary pull request or merged as though it were one feature commit.

Both configured push URLs are disabled in this checkout until the publication
strategy is approved.

## Recommended path: preserve the existing public project

Keeping the existing repository retains its users, issues, stars, package name
and discoverability. Treat the Rust version as a deliberate major rewrite:

1. Fetch the exact remote Clojure `main` tip and record its commit ID.
2. Create and push an annotated final Clojure tag and a permanent
   `clojure-legacy` branch at that exact tip.
3. Export a repository bundle and verify the archive branch/tag from a fresh
   clone.
4. Complete `docs/chaptarr/RELEASE_CHECKLIST.md` on a disposable Chaptarr
   instance.
5. Re-enable the `origin` push URL only for the migration window.
6. With branch protection handled intentionally, replace `main` using
   `--force-with-lease` pinned to the previously recorded old tip. Never use an
   unguarded force push.
7. Restore branch protection, confirm the Rust `main` workflow, discussions,
   security reporting and GHCR package links, then publish the beta tag.
8. Keep the Clojure archive branch/tag indefinitely and explain the rewrite in
   the release notes.

This is the recommended option because the user-facing product is still
DoplarrChaptarr even though its implementation language and upstream base have
changed.

## Lower-risk Git option: create a second repository

Create a new repository such as `DoplarrChaptarr-rs`, set it as `origin`, and
publish this history normally. This avoids rewriting `main`, but splits users,
issues, release history and container naming across two projects. If chosen,
update every repository and GHCR URL before the first release.

No remote branch, tag, release or container should be published until one path
is explicitly selected.
