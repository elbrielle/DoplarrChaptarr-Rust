# Migrating from Clojure Doplarr or DoplarrChaptarr

The Rust version is a complete rewrite based on `activexray/doplarr_rs`. It
keeps the old Doplarr movie/TV flow and restores DoplarrChaptarr's `/request
book` and `/request audiobook` commands with a new TOML configuration.

## Config format: EDN → TOML

The old bot read an EDN file with namespaced keys. The new bot reads a TOML file where each backend is its own `[[backends]]` table.

**Before (config.edn):**
```edn
{:sonarr/url "http://localhost:8989"
 :sonarr/api "your_sonarr_api_key"
 :radarr/url "http://localhost:7878"
 :radarr/api "your_radarr_api_key"
 :discord/token "your_discord_token"}
```

**After (config.toml):**
```toml
discord_token = "your_discord_token"

[[backends]]
media = "series"
[backends.config.Sonarr]
url = "http://localhost:8989"
api_key = "your_sonarr_api_key"

[[backends]]
media = "movie"
[backends.config.Radarr]
url = "http://localhost:7878"
api_key = "your_radarr_api_key"
```

The `media` field sets the name of the `/request <media>` slash command. You can name it anything you want — `series`, `tv`, `movie`, `film`, etc.

## Environment variables (no config file)

If you ran the Clojure bot with **only environment variables and no mounted config**, that keeps working — the Rust bot detects the same legacy variables on startup, builds a config from them, and runs. No config file or volume required.

| Setting | Variable |
|---|---|
| Discord token | `DISCORD__TOKEN` |
| Seerr / Overseerr | `OVERSEERR__URL`, `OVERSEERR__API`, `OVERSEERR__DEFAULT_ID` |
| Sonarr | `SONARR__URL`, `SONARR__API` |
| Radarr | `RADARR__URL`, `RADARR__API` |
| Chaptarr connection | `CHAPTARR__URL`, `CHAPTARR__API` |
| Chaptarr roots | `CHAPTARR__EBOOK_ROOTFOLDER`, `CHAPTARR__AUDIOBOOK_ROOTFOLDER` |
| Chaptarr quality profiles | `CHAPTARR__EBOOK_QUALITY_PROFILE`, `CHAPTARR__AUDIOBOOK_QUALITY_PROFILE` |
| Chaptarr metadata profiles | `CHAPTARR__EBOOK_METADATA_PROFILE`, `CHAPTARR__AUDIOBOOK_METADATA_PROFILE` |
| Public request message | `DISCORD__REQUESTED_MSG_STYLE` (`:none` becomes `public_followup = false`) |
| Log level | `LOG_LEVEL` |

When `CHAPTARR__URL` and `CHAPTARR__API` are present, migration generates both
the `book`/`ebook` and `audiobook`/`audiobook` backend entries. Any of the six
old Chaptarr root/profile variables that are present are preserved as `${...}`
references on both entries, so secrets and deployment-specific paths do not get
copied as literal values. Chaptarr coexists with Seerr, Sonarr, and Radarr.

Other per-backend options should be set in a mounted config file. Doplarr writes
the generated `config.toml` when it can, which lets you inspect and extend the
migration rather than relying on hidden conversion behavior.

> [!IMPORTANT]
> **Overseerr generates two commands and takes precedence.** Mirroring the Clojure bot, `OVERSEERR__*` produces separate `movie` and `series` commands (two `[[backends]]` entries with `media_filter = "movie"` / `media_filter = "tv"`). Because Overseerr fronts Sonarr/Radarr, the `SONARR__*`/`RADARR__*` variables are **ignored** when `OVERSEERR__*` is set (with a note logged at startup). Set up the direct `SONARR__*`/`RADARR__*` backends only when you're not using Overseerr.

You can also reference environment variables from anywhere in a config file with `${VAR}`:

```toml
[backends.config.Seerr]
url = "${OVERSEERR__URL}"
api_key = "${OVERSEERR__API}"
```

## Option mapping

### Global

| Old key | New key | Notes |
|---|---|---|
| `:discord/token` | `discord_token` | Top-level string |
| `:log-level` | `log_level` | String instead of keyword — e.g. `"info"` instead of `:info` |
| `:discord/requested-msg-style` | `public_followup` | `:none` → `false`; `:plain` or `:embed` → `true` (default). The embed/plain distinction is gone. |
| `:discord/max-results` | *(removed)* | Fixed at Discord's 25-item autocomplete limit |

### Sonarr

| Old key | New key | Notes |
|---|---|---|
| `:sonarr/url` | `url` | Under `[backends.config.Sonarr]` |
| `:sonarr/api` | `api_key` | Renamed from `api` |
| `:sonarr/quality-profile` | `quality_profile` | Optional; prompts user if omitted |
| `:sonarr/rootfolder` | `rootfolder` | Optional; prompts user if omitted |
| `:sonarr/season-folders` | `season_folders` | Optional |
| `:sonarr/language-profile` | *(removed)* | Sonarr v4 dropped language profiles |
| `:partial-seasons` | *(removed)* | The season selection UI no longer offers a partial-season flow |

### Radarr

| Old key | New key | Notes |
|---|---|---|
| `:radarr/url` | `url` | Under `[backends.config.Radarr]` |
| `:radarr/api` | `api_key` | Renamed from `api` |
| `:radarr/quality-profile` | `quality_profile` | Optional; prompts user if omitted |
| `:radarr/rootfolder` | `rootfolder` | Optional; prompts user if omitted |

### Overseerr → Seerr

The backend has been moved to `Seerr` (covers both Overseerr and Jellyseerr):

| Old key | New key | Notes |
|---|---|---|
| `:overseerr/url` | `url` | Under `[backends.config.Seerr]` |
| `:overseerr/api` | `api_key` | Renamed from `api` |
| `:overseerr/default-id` | `fallback_user_id` | Same semantics — Seerr user ID for unlinked Discord users |

### Chaptarr

Each format is now an explicit backend. The two entries produce the same slash
commands as the Clojure fork:

```toml
[[backends]]
media = "book"
[backends.config.Chaptarr]
url = "${CHAPTARR__URL}"
api_key = "${CHAPTARR__API}"
format = "ebook"

[[backends]]
media = "audiobook"
[backends.config.Chaptarr]
url = "${CHAPTARR__URL}"
api_key = "${CHAPTARR__API}"
format = "audiobook"
```

| Old key | New key | Notes |
|---|---|---|
| `:chaptarr/url` | `url` | Required under `[backends.config.Chaptarr]` |
| `:chaptarr/api` | `api_key` | Required; renamed from `api` |
| *(command-specific)* | `format` | Required: `ebook` or `audiobook` |
| `:chaptarr/ebook-rootfolder` | `ebook_rootfolder` | Optional exact Chaptarr path |
| `:chaptarr/audiobook-rootfolder` | `audiobook_rootfolder` | Optional exact Chaptarr path |
| `:chaptarr/ebook-quality-profile` | `ebook_quality_profile` | Optional exact profile name |
| `:chaptarr/audiobook-quality-profile` | `audiobook_quality_profile` | Optional exact profile name |
| `:chaptarr/ebook-metadata-profile` | `ebook_metadata_profile` | Optional exact profile name |
| `:chaptarr/audiobook-metadata-profile` | `audiobook_metadata_profile` | Optional exact profile name |
| *(new)* | `openlibrary_covers` | Optional; defaults to `true`; disable to keep search text inside Chaptarr |

Both entries may include all six optional root/profile fields. Chaptarr needs
both formats' defaults when an author is first created, even though each command
monitors and searches only its configured format. An omitted value is selected
automatically only when Chaptarr exposes one valid option; ambiguous omissions
fail startup and must be configured explicitly.

The user-visible flow stays familiar: search for a work, select a result,
review it, then press **Request**. The Rust successor deliberately does not add
authors or books while the user is browsing or confirming. Cover images are
best-effort and cannot block a valid request. When `openlibrary_covers = true`,
a coverless search sends its search text to Open Library's public API; results
are cached and rate-limited.

> [!WARNING]
> Chaptarr's source and a stable public API specification are not currently
> available. The initial contract targets captured, sanitized Chaptarr
> `0.9.720.0` responses. Pin both applications to versions you have tested
> together. Read release notes and test search plus one request before promoting
> upgrades to other users.

## New options

These have no equivalent in the Clojure version:

| Key | Backend | Description |
|---|---|---|
| `monitor_type` | Radarr | Lock all requests to a specific monitor mode instead of prompting |
| `minimum_availability` | Radarr | Pre-set minimum availability instead of prompting |
| `series_type` | Sonarr | Force `standard`, `daily`, or `anime`; omit to auto-detect from genres |
| `allow_specials` | Sonarr | Offer Season 0 in the season picker |
| `allow_all_seasons` | Sonarr, Seerr | Offer an "All Seasons" option (all current + future seasons); default true |
| `allow_4k` | Seerr | Show a Standard/4K quality choice at request time |
| `openlibrary_covers` | Chaptarr | Enrich missing covers through Open Library; defaults to true |

You can also point multiple `[[backends]]` entries at the same Radarr or Sonarr instance with different settings to create separate commands — e.g. `/request movie` and `/request movie_4k` from one Radarr instance.

Do not migrate the old container straight to
`ghcr.io/activexray/doplarr_rs:latest`: upstream Rust Doplarr does not include
the Chaptarr provider, and `latest` is not a reproducible deployment. Build this
successor from a pinned commit during development:

```bash
cargo build --release --locked
./target/release/doplarr ./config.toml
```

Once numbered DoplarrChaptarr releases are published, deploy an exact release
tag or image digest and keep the previous working artifact available for
rollback. The bot reads the TOML path passed as its first argument and defaults
to `./config.toml` when no path is supplied.

Run the candidate with `--check` before replacing the old Discord container:

```bash
docker compose run --rm --no-deps doplarrchaptarr --check /config.toml
```

This loads the same configuration and connects to every selected backend, but
returns before a Discord client or gateway is created. Treat a successful
`"discord": "not_contacted"` report as backend/configuration proof, not as a
substitute for the later interactive search and request checks. A missing
config file or an untested Chaptarr version is a hard preflight failure.

For the initial release, the pinned image reference is:

```text
ghcr.io/elbrielle/doplarrchaptarr-rust:v4.6.0-chaptarr.1
```

See [config.example.toml](config.example.toml) for a full annotated reference.
