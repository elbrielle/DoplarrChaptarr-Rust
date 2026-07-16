# Chaptarr beta release checklist

Use this checklist before publishing a DoplarrChaptarr build against a new
Chaptarr version. Chaptarr's API is private and pre-1.0, so unit fixtures prove
the client contract but cannot prove that writes still persist.

The Chaptarr provider is a single-work flow. This checklist does not approve
automatic series expansion. Results with clear multi-book title signals - such
as a title ending in `bundle` or `trilogy`, an `omnibus`, a box set, a
`complete ... series`, or an explicit numbered book collection/set - must be
rejected with an instruction to request individual titles.

## Preconditions

- Use a disposable Chaptarr instance or a disposable author/library root. Do
  not run mutation tests against a family production library.
- Record the exact DoplarrChaptarr commit, candidate-image digest, Chaptarr
  version and Chaptarr container digest. Test that exact candidate image.
- Back up Chaptarr's database and configuration and verify the restore path.
- Use synthetic or explicitly approved test books in both formats.
- Keep Open Library enrichment disabled if test search text must remain local.

## Automated gates

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- --deny warnings`
- `cargo build --release -p doplarr --locked`
- `nix flake check`
- Validate `docker compose config` without pasting its expanded secret values.
- Build the exact container with `nix build .#dockerImage`, load it, and run
  `.github/ci/smoke-image.sh IMAGE`; retain the resulting commit-specific image
  artifact and checksum.
- Run the candidate with `--check /config.toml` against the disposable
  Chaptarr instance and save its sanitized `discord: not_contacted` report.

## Read-only interaction proof

1. After preflight passes, start both `/request book` and `/request audiobook`
   backends.
2. Search each format and open the confirmation screen.
3. Abandon both interactions without pressing **Request**.
4. Confirm Chaptarr has no new author, book, monitor change or queued command.
5. Confirm covers render when available and a missing cover does not block the
   interaction.

## Mutation proof

Run each case once and inspect Chaptarr after every step:

1. Request an ebook for a new author.
2. Request an audiobook for a different new author.
3. Request an exact missing work under an already-local author.
4. Request the other format for a work whose first format is already local.
5. Repeat one request concurrently from two Discord users, then repeat it once
   immediately after a valid acknowledgement in the same bot process.
6. Select a result with a clear multi-book signal and press **Request**; verify
   the bot names the single-work limitation and makes no mutation.
7. In the disposable instance, prepare a partial state with the chosen edition
   and book monitored but no confirmed `BookSearch`, then repeat the request.
8. Inject or simulate a failed command poll during new-author settling and
   verify the request fails closed before edition, monitor, or search writes.
9. Complete a search with zero results and no grab/file, or restart the bot after
   its recent acknowledgement is lost, then explicitly retry; verify at most one
   fresh search is queued by that new attempt.

For every case, verify:

- the selected title, author, `foreignBookId` when present and `mediaType` match;
- no sibling work or unrequested edition was monitored;
- authoritative edition `format` matches the requested format, and a Physical
  edition is never selected for an audiobook request;
- the unrequested format's author gate did not change;
- top-level book monitoring AND the explicit requested-format monitor flag were
  both read back true;
- exactly one requested-format edition was monitored and matched the chosen ID;
- exactly one acknowledged `BookSearch` was queued after strict read-back;
- a concurrent request, exact active `BookSearch`, and immediate same-process
  retry after a valid acknowledgement did not queue another search;
- a retry repaired partial state instead of reporting it already requested;
- after restart or a completed zero-result/no-grab search, an explicit retry may
  queue one fresh search and does not queue more than one for that attempt;
- settle polling errors/timeouts produced a clear failure and no downstream
  mutation;
- a missing exact row did not queue `RefreshAuthor`;
- an exact unresolved placeholder may queue at most one `RefreshAuthor`, and a
  still-unresolved row never reaches `BookSearch`.

## Promotion record

Save a sanitized release note with the versions, digest, cases run, results and
rollback artifact. Remove API keys, Discord IDs/tokens, search titles, local
paths and internal hostnames. Only after this record is complete should a beta
be published; only a later explicitly stable tag may move the `latest` image.
