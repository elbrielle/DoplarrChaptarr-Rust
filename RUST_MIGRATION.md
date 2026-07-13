# Publishing the Rust rewrite

## Decision

The Rust successor is published as its own project:

- Repository: `https://github.com/elbrielle/DoplarrChaptarr-Rust`
- Container: `ghcr.io/elbrielle/doplarrchaptarr-rust`
- Upstream: `https://github.com/activexray/doplarr_rs`

This checkout and the original Clojure DoplarrChaptarr repository have
unrelated Git histories. Keeping them separate avoids a destructive default
branch replacement, preserves the original releases and source exactly, and
makes future Rust Doplarr updates straightforward to compare and merge.

## Legacy repository role

The original `elbrielle/DoplarrChaptarr` repository remains the historical
Clojure implementation. Its README and repository description point visitors
to the Rust successor. Existing Clojure tags, branches, issues and packages are
retained rather than rewritten.

Do not push Rust commits into the Clojure repository or describe this rewrite
as an ordinary pull request. Security reports and new feature work belong in
the Rust repository; old Clojure issues may be closed or redirected as time
allows.

## Release boundary

Publishing the source repository does not publish a beta binary or container.
The moving `main` container job is additionally gated by the repository variable
`PUBLISH_MAIN_IMAGE=true`, which should remain unset until live validation.
Ordinary code CI still builds and smoke-tests the exact container, then retains
the SHA-tagged image tar and checksum as a 14-day workflow artifact for owner
testing. That artifact is not a public package. When publication is enabled,
the gated job downloads and pushes that already-tested image rather than
building a second potentially different artifact.
Before creating `v4.6.0-chaptarr.1`, complete
`docs/chaptarr/RELEASE_CHECKLIST.md` against a disposable Chaptarr instance.
The release workflow treats hyphenated versions as prereleases and does not
move `latest`; only an explicitly stable tag may do that.

For ongoing maintenance, fetch `upstream/main`, review its changes, and merge
or rebase intentionally on a feature branch. Preserve upstream authorship and
license notices while authoring DoplarrChaptarr-specific commits as Elisha
Lucero.
