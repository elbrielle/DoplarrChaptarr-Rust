<p align="center">
  <img src="logos/logo-with-text.svg" alt="Doplarr" width="480">
</p>

<p align="center">
  <a href="LICENSE-GPL-3.0"><img alt="License" src="https://img.shields.io/badge/license-GPL--3.0-blue?style=for-the-badge"></a>
</p>

# DoplarrChaptarr (Rust)

A Discord bot for requesting movies, television, ebooks, and audiobooks through
\*arr-style backends, written in Rust. This is the Rust successor to the original
Clojure DoplarrChaptarr fork and is based on the rewritten Rust Doplarr.

The original Clojure implementation remains available in
[`elbrielle/DoplarrChaptarr`](https://github.com/elbrielle/DoplarrChaptarr).
This repository is the maintained successor. Source may be published before a
binary beta; releases remain gated by the disposable Chaptarr checklist below.

Each backend creates a `/request <media>` slash command. Chaptarr adds the same
two public commands as the old fork: `/request book` and `/request audiobook`.

## Screenshots

<p align="center">
  <img src="screenshots/series.png" alt="Series selection interface" width="400">
</p>

## Setup

### 1. Create a Discord bot

Go to the [Discord Developer Portal](https://discord.com/developers/applications), create a new application, then open the **Bot** tab and create a bot. Copy the token — you'll need it in your config.

Under **OAuth2 → URL Generator**, tick the `bot` and `applications.commands` scopes. Open the generated URL to invite the bot to your server.

> [!NOTE]
> To post request confirmations in the channel for everyone to see, also tick the `Send Messages` permission. Without it, requests still work — the public announcement is just skipped.

### 2. Get your backend API keys

- **Sonarr / Radarr**: Settings → General → Security → API Key
- **Seerr**: Settings → API Key — must be an **admin** key
- **Chaptarr**: Settings → General → Security → API Key

### 3. Configure and run

Create a `config.toml` (see [Configuration](#configuration) below). From this
project checkout, build and run the current code with:

```bash
cargo build --release --locked
./target/release/doplarr ./config.toml
```

For a server deployment, use a numbered release rather than a moving `latest`
container tag. Pinning a release tag—or, more strictly, an image digest—keeps a
Chaptarr API change from arriving at the same time as an unrelated bot update.
After the first release is published, the equivalent pinned Compose service is:

```yaml
services:
  doplarrchaptarr:
    image: ghcr.io/elbrielle/doplarrchaptarr-rust:v4.6.0-chaptarr.1
    container_name: doplarrchaptarr
    restart: unless-stopped
    init: true
    read_only: true
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    env_file:
      - .env
    volumes:
      - type: bind
        source: ./config.toml
        target: /config.toml
        read_only: true
        bind:
          create_host_path: false
    tmpfs:
      - /tmp:size=16m,mode=1777
```

Copy `.env.example` to `.env`, make `discord_token` and backend keys in
`config.toml` reference those environment names, then run `chmod 600 .env` and
`chmod 644 config.toml`. The config is readable by the container's unprivileged
user but contains no secret values; Docker reads `.env` on the host and passes
the values into the process. The checked-in `docker-compose.yml` contains the
same non-root, read-only defaults. Never commit `.env`, a populated config, or
the output of `docker compose config`, which can expand environment values.

Commands register automatically on startup. If they don't appear right away, wait a minute or restart your Discord client.

## Configuration

Doplarr reads a single `config.toml`. A minimal one looks like this:

```toml
discord_token = "YOUR_DISCORD_BOT_TOKEN"

# Movies via Radarr → /request movie
[[backends]]
media = "movie"

[backends.config.Radarr]
url = "http://localhost:7878"
api_key = "your_radarr_api_key"

# TV via Sonarr → /request series
[[backends]]
media = "series"

[backends.config.Sonarr]
url = "http://localhost:8989"
api_key = "your_sonarr_api_key"
```

Each `[[backends]]` block adds one `/request <media>` command. Sonarr and Radarr
can ask for omitted choices in Discord. Chaptarr instead resolves its six
root/profile defaults at startup: it auto-selects an omitted value only when
there is exactly one valid option and otherwise asks the operator to configure
the ambiguous field explicitly.

That's all most setups need. For the **full list of options** — plus Chaptarr,
Seerr, 4K, anime, and pointing several commands at one instance — see the
annotated **[config.example.toml](config.example.toml)**.

> [!TIP]
> - **No config yet?** Start the bot without one and it writes a starter
>   `config.toml` for you to edit.
> - **Keep secrets out of the file** by referencing environment variables:
>   `api_key = "${RADARR_API_KEY}"`.
> - **Coming from the Clojure Doplarr?** Your old environment variables still
>   work with no config file at all — see **[MIGRATING.md](MIGRATING.md)**.

### Using Seerr

> [!IMPORTANT]
> If you request through Seerr/Overseerr/Jellyseerr, each user must link their
> Discord account, or their requests are rejected. In Seerr, enable the Discord
> notification agent (Settings → Notifications → Discord), then each user enters
> their Discord User ID on their profile. To accept requests from unlinked users
> instead, set `fallback_user_id` in the config.

### Using Chaptarr

Add both backend entries to provide ebook and audiobook requests:

```toml
[[backends]]
media = "book"

[backends.config.Chaptarr]
url = "http://chaptarr:8789"
api_key = "${CHAPTARR_API_KEY}"
format = "ebook"
ebook_rootfolder = "/books"
audiobook_rootfolder = "/audiobooks"
ebook_quality_profile = "E-Book"
audiobook_quality_profile = "Audiobook"
ebook_metadata_profile = "Ebook Default"
audiobook_metadata_profile = "Audiobook Default"
openlibrary_covers = true

[[backends]]
media = "audiobook"

[backends.config.Chaptarr]
url = "http://chaptarr:8789"
api_key = "${CHAPTARR_API_KEY}"
format = "audiobook"
ebook_rootfolder = "/books"
audiobook_rootfolder = "/audiobooks"
ebook_quality_profile = "E-Book"
audiobook_quality_profile = "Audiobook"
ebook_metadata_profile = "Ebook Default"
audiobook_metadata_profile = "Audiobook Default"
openlibrary_covers = true
```

The six root/profile fields are optional. Doplarr auto-selects an omitted value
only when Chaptarr exposes exactly one valid option; an ambiguous choice stops
startup with a configuration error rather than silently choosing a library.
When configured, every path and profile name must match Chaptarr exactly. Both
backend entries need visibility of both formats because Chaptarr initializes a
new author with ebook and audiobook settings before monitoring the requested
work.

Search is read-only through the confirmation screen. Selecting a result does
not add an author or book; Chaptarr is mutated only after the requester presses
**Request**. Covers are resolved from the metadata returned by Chaptarr, with a
bounded Open Library fallback, but remain best-effort: a request must still work
when a cover host is unavailable or a title has no usable image.

`openlibrary_covers` defaults to `true`. When enabled, a search whose Chaptarr
results lack usable covers sends the search text to Open Library's public Search
API. Results are cached and globally rate-limited. Set it to `false` on both
backends if you do not want search text sent to that service.

> [!WARNING]
> Chaptarr's source and a stable public API specification are not currently
> available. The initial integration contract is tested against captured,
> sanitized Chaptarr `0.9.720.0` API responses, but an update can still change
> the private API. Pin known
> working DoplarrChaptarr and Chaptarr versions, read release notes before
> upgrading, and report the DoplarrChaptarr version, Chaptarr version, endpoint,
> and sanitized response shape when filing a compatibility issue.

The endpoints, response fields, search rules, cover fallbacks, and safe write
sequence are documented in
**[docs/chaptarr/COMPATIBILITY.md](docs/chaptarr/COMPATIBILITY.md)**. Maintainers
should complete the
**[beta release checklist](docs/chaptarr/RELEASE_CHECKLIST.md)** before
publishing against a new Chaptarr version.

## Running as a Service

```ini
# /etc/systemd/system/doplarr.service
[Unit]
Description=DoplarrChaptarr Discord Bot
After=network.target

[Service]
Type=simple
User=doplarr
Group=doplarr
WorkingDirectory=/opt/doplarr
EnvironmentFile=/opt/doplarr/.env
ExecStart=/opt/doplarr/doplarr /opt/doplarr/config.toml
Restart=on-failure
RestartSec=5
UMask=0077
NoNewPrivileges=true
CapabilityBoundingSet=
PrivateDevices=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo chown doplarr:doplarr /opt/doplarr/config.toml /opt/doplarr/.env
sudo chmod 600 /opt/doplarr/config.toml /opt/doplarr/.env
sudo systemctl daemon-reload
sudo systemctl enable --now doplarr
```

## Building from Source

**With Nix:**

```bash
nix build
nix run . /path/to/config.toml
```

**With Cargo** (requires the pinned Rust toolchain):

```bash
cargo build --release --locked
./target/release/doplarr /path/to/config.toml
```

## Troubleshooting

**Bot doesn't respond to commands**
- Make sure you invited the bot with both `bot` and `applications.commands` scopes
- Commands register on startup — wait a minute or restart Discord if they're missing
- Check logs for connection errors

**Backend connection errors**
- Test your API keys directly in the \*arr web UI
- If running in Docker, make sure the container can reach your \*arr services (check network/hostname)
- Root-folder paths and profile names are case-sensitive and must match the backend exactly

**Chaptarr search or request errors**
- Confirm both `/request book` and `/request audiobook` have their own backend entry and format
- Check the Chaptarr version against the versions listed in this project's release notes
- A missing cover is not a request failure; inspect logs only if the search or request itself stops
- If an update changed the API shape, roll back to your last pinned working version before collecting sanitized diagnostics

**Seerr: "user not found" or requests rejected**
1. Enable the Discord notification agent in Seerr (Settings → Notifications → Discord)
2. Each user goes to their Seerr profile → Settings → Notifications → Discord and enters their Discord User ID
3. Or set `fallback_user_id` in your config to accept requests from unlinked users

**Config parse errors**
- Validate your TOML syntax (e.g. [jsonformatter.org/toml-validator](https://jsonformatter.org/toml-validator))
- `discord_token` and at least one `[[backends]]` entry are required
- Each backend's `media` value must be unique

## Migrating from the Clojure version

See [MIGRATING.md](MIGRATING.md) for the full mapping from the old Clojure
Doplarr and DoplarrChaptarr configuration to TOML, including the six existing
`CHAPTARR__*` root/profile variables. The separate-repository decision and
upstream maintenance boundary are recorded in
[RUST_MIGRATION.md](RUST_MIGRATION.md).

## Development

See [README_DEVELOPER.md](README_DEVELOPER.md) for adding new backends, generating API bindings, and contributing.

## License

DoplarrChaptarr is distributed under GPL-3.0-only because the executable links
generated Sonarr and Radarr clients declared GPL-3.0. Portions inherited from
Rust Doplarr remain available under their original MIT or Apache-2.0 terms;
those notices and license files are preserved. See [LICENSING.md](LICENSING.md)
and [LICENSE-GPL-3.0](LICENSE-GPL-3.0).

## Acknowledgments

- [activexray/doplarr_rs](https://github.com/activexray/doplarr_rs) for the Rust rewrite and backend architecture
- [Twilight](https://github.com/twilight-rs/twilight) for Discord API bindings
- [OpenAPI Generator](https://github.com/OpenAPITools/openapi-generator) for \*arr API clients
