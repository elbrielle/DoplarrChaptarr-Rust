//! Headless canary driver for the RELEASE_CHECKLIST mutation proof.
//!
//! Dev-only (`cargo run -p doplarr --features canary --bin chaptarr_canary`);
//! the `canary` feature keeps it out of the release binary set. Both backends
//! are constructed through `startup::connect_backends` — the exact production
//! path, including startup profile/root resolution — and each checklist case
//! drives `MediaBackend::search` / `additional_details` / `request` directly,
//! then verifies the checklist bullets with read-only GETs.
//!
//! Transcripts are sanitized by construction: the API key travels only in the
//! `X-Api-Key` header (env `CHAPTARR_API_KEY`, never a flag or a URL), so no
//! logged request line can carry it. Only run this against the disposable
//! canary instance described in `docs/chaptarr/SPRINT-3.md` §4.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use doplarr::{
    config::{Backend, BackendConfig, ChaptarrFormat, Config},
    providers::{MediaBackend, MediaItem, UserFacingError},
    startup,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Ebook,
    Audiobook,
}

impl Format {
    fn media(self) -> &'static str {
        match self {
            Format::Ebook => "book",
            Format::Audiobook => "audiobook",
        }
    }

    fn monitored_flag(self) -> &'static str {
        match self {
            Format::Ebook => "ebookMonitored",
            Format::Audiobook => "audiobookMonitored",
        }
    }

    fn gate_flag(self) -> &'static str {
        match self {
            Format::Ebook => "ebookMonitorFuture",
            Format::Audiobook => "audiobookMonitorFuture",
        }
    }

    fn other_gate_flag(self) -> &'static str {
        match self {
            Format::Ebook => "audiobookMonitorFuture",
            Format::Audiobook => "ebookMonitorFuture",
        }
    }

    /// `readingFormatId` values per the compatibility contract: 2 = audio,
    /// 3 = ebook, 1 = physical (never selectable).
    fn reading_format_id(self) -> i64 {
        match self {
            Format::Ebook => 3,
            Format::Audiobook => 2,
        }
    }

    fn media_type(self) -> &'static str {
        match self {
            Format::Ebook => "ebook",
            Format::Audiobook => "audiobook",
        }
    }
}

#[derive(Parser)]
#[command(
    name = "chaptarr_canary",
    about = "Drive the RELEASE_CHECKLIST mutation cases against a disposable Chaptarr instance"
)]
struct Cli {
    /// Chaptarr base URL (or env CHAPTARR_URL). The API key comes from env
    /// CHAPTARR_API_KEY only, so it cannot leak into shell history.
    #[arg(long)]
    url: Option<String>,
    #[command(subcommand)]
    case: Case,
}

#[derive(Subcommand)]
enum Case {
    /// Connect both backends exactly as production `--check` does and print
    /// the sanitized preflight report.
    Check,
    /// One full request pipeline plus verification (checklist cases 1-4 and
    /// 9-11 are this command against different instance state).
    Request {
        #[arg(long)]
        format: Format,
        #[arg(long)]
        title: String,
        /// Substring picking a specific search result (default: best match).
        #[arg(long)]
        select: Option<String>,
        /// Expect a user-facing refusal containing this text instead of a
        /// completed request (e.g. "already available").
        #[arg(long)]
        expect_message: Option<String>,
    },
    /// Checklist case 5: two concurrent requests with distinct requester
    /// ids, then an immediate same-process retry after the acknowledgement.
    Concurrent {
        #[arg(long)]
        format: Format,
        #[arg(long)]
        title: String,
        #[arg(long)]
        select: Option<String>,
    },
    /// Checklist case 6: select a clear multi-book result and press
    /// Request; asserts the single-work refusal and zero mutations.
    RejectBundle {
        #[arg(long)]
        format: Format,
        #[arg(long)]
        title: String,
        #[arg(long)]
        select: Option<String>,
    },
    /// Checklist case 7 preparation: put an existing unmonitored row into
    /// the partial state (edition + book monitored, author gate open, no
    /// BookSearch) via direct API writes. Follow with `request` in a fresh
    /// process to prove repair.
    PreparePartial {
        #[arg(long)]
        format: Format,
        #[arg(long)]
        author_id: i64,
        /// Substring picking a specific row of that author (default: first
        /// unmonitored row with a usable requested-format edition).
        #[arg(long)]
        select: Option<String>,
    },
    /// Checklist case 8, half one: run a request that must fail closed at
    /// the settle deadline (stop the container once the add is through).
    SettleFailure {
        #[arg(long)]
        format: Format,
        #[arg(long)]
        title: String,
        #[arg(long)]
        select: Option<String>,
    },
    /// Checklist case 8, half two, after the container is back: verify the
    /// failed request left no downstream write behind.
    VerifyUntouched {
        #[arg(long)]
        format: Format,
        /// Author name (substring, case-insensitive) of the aborted add.
        #[arg(long)]
        author: String,
    },
    /// Checklist case 12: probe POST /book/{id}/editions/wanted per
    /// decision record 0001 on disposable rows.
    ProbeWanted {
        #[arg(long)]
        audiobook_book_id: i64,
        #[arg(long)]
        ebook_book_id: i64,
    },
    /// Read-only state dump for manual inspection.
    State {
        #[arg(long)]
        author_id: Option<i64>,
        #[arg(long)]
        book_id: Option<i64>,
    },
}

fn env_var(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

fn backend_config(url: &str, api_key: &str, format: ChaptarrFormat) -> Backend {
    Backend {
        media: match format {
            ChaptarrFormat::Ebook => "book".into(),
            ChaptarrFormat::Audiobook => "audiobook".into(),
        },
        config: BackendConfig::Chaptarr {
            url: url.into(),
            api_key: api_key.into(),
            format,
            ebook_rootfolder: None,
            audiobook_rootfolder: None,
            ebook_quality_profile: None,
            audiobook_quality_profile: None,
            ebook_metadata_profile: None,
            audiobook_metadata_profile: None,
            // The canary must never send search text to Open Library.
            openlibrary_covers: Some(false),
        },
    }
}

async fn connect(url: &str, api_key: &str) -> Result<startup::ConnectedBackends> {
    let config = Config {
        log_level: None,
        public_followup: None,
        discord_token: String::new(),
        backends: vec![
            backend_config(url, api_key, ChaptarrFormat::Ebook),
            backend_config(url, api_key, ChaptarrFormat::Audiobook),
        ],
    };
    startup::connect_backends(&config).await
}

/// Read-mostly raw client for verification. Verification never mutates; the
/// only raw writes live in `prepare_partial` (checklist case 7 prep) and
/// `probe_wanted` (case 12), both explicitly sanctioned by the checklist.
struct RawApi {
    base: String,
    key: String,
    client: reqwest::Client,
}

impl RawApi {
    fn new(url: &str, key: &str) -> Result<Self> {
        let url = url.trim_end_matches('/');
        let base = if url.ends_with("/api/v1") {
            url.to_string()
        } else {
            format!("{url}/api/v1")
        };
        Ok(Self {
            base,
            key: key.to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
        })
    }

    async fn get(&self, path: &str) -> Result<Value> {
        info!("verify GET {path}");
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .header("X-Api-Key", &self.key)
            .send()
            .await
            .with_context(|| format!("GET {path} failed"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .with_context(|| format!("GET {path} returned non-JSON"))?;
        if !status.is_success() {
            bail!("GET {path} returned {status}: {body}");
        }
        Ok(body)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<(u16, Value)> {
        info!("probe {method} {path}");
        let response = self
            .client
            .request(method.clone(), format!("{}{path}", self.base))
            .header("X-Api-Key", &self.key)
            .json(body)
            .send()
            .await
            .with_context(|| format!("{method} {path} failed"))?;
        let status = response.status().as_u16();
        let body = response.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn field_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn field_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn field_id(value: &Value) -> Option<i64> {
    value.get("id").and_then(Value::as_i64).filter(|id| *id > 0)
}

#[derive(Debug, Clone, PartialEq)]
struct AuthorGates {
    name: String,
    monitored: bool,
    ebook_future: bool,
    audiobook_future: bool,
    add_options_present: bool,
}

struct Snapshot {
    authors: BTreeMap<i64, AuthorGates>,
    command_ids: BTreeSet<i64>,
    /// book id -> (monitored, ebookMonitored, audiobookMonitored)
    books: BTreeMap<i64, (bool, bool, bool)>,
}

async fn take_snapshot(api: &RawApi) -> Result<Snapshot> {
    let authors_raw = api.get("/author").await?;
    let mut authors = BTreeMap::new();
    let mut books = BTreeMap::new();
    for author in authors_raw.as_array().into_iter().flatten() {
        let Some(id) = field_id(author) else { continue };
        authors.insert(
            id,
            AuthorGates {
                name: field_str(author, "authorName"),
                monitored: field_bool(author, "monitored"),
                ebook_future: field_bool(author, "ebookMonitorFuture"),
                audiobook_future: field_bool(author, "audiobookMonitorFuture"),
                add_options_present: author.get("addOptions").is_some(),
            },
        );
        let rows = api.get(&format!("/book?authorId={id}")).await?;
        for row in rows.as_array().into_iter().flatten() {
            let Some(book_id) = field_id(row) else {
                continue;
            };
            books.insert(
                book_id,
                (
                    field_bool(row, "monitored"),
                    field_bool(row, "ebookMonitored"),
                    field_bool(row, "audiobookMonitored"),
                ),
            );
        }
    }
    let commands = api.get("/command").await?;
    let command_ids = commands
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(field_id)
        .collect();
    Ok(Snapshot {
        authors,
        command_ids,
        books,
    })
}

fn command_name(command: &Value) -> String {
    let name = field_str(command, "name");
    if name.is_empty() {
        field_str(command, "commandName")
    } else {
        name
    }
}

fn referenced_book_ids(command: &Value) -> Vec<i64> {
    let mut ids = Vec::new();
    for scope in [command.get("body"), Some(command)].into_iter().flatten() {
        if let Some(id) = scope
            .get("bookId")
            .and_then(Value::as_i64)
            .filter(|id| *id > 0)
        {
            ids.push(id);
        }
        for id in scope
            .get("bookIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = id.as_i64().filter(|id| *id > 0) {
                ids.push(id);
            }
        }
    }
    ids
}

/// A per-case report: everything the evidence record needs, with an explicit
/// PASS/FAIL per checklist bullet. Failing any bullet fails the process.
struct Report {
    passed: bool,
}

impl Report {
    fn new() -> Self {
        Report { passed: true }
    }

    fn check(&mut self, ok: bool, bullet: &str) {
        println!("  [{}] {bullet}", if ok { "PASS" } else { "FAIL" });
        self.passed &= ok;
    }

    fn note(&self, text: &str) {
        println!("  [note] {text}");
    }
}

enum Outcome {
    Success,
    /// A `UserFacingError` — an expected, user-actionable refusal.
    Refusal(String),
    Error(String),
}

impl Outcome {
    fn describe(&self) -> String {
        match self {
            Outcome::Success => "SUCCESS".into(),
            Outcome::Refusal(message) => format!("USER-FACING: {message}"),
            Outcome::Error(message) => format!("ERROR: {message}"),
        }
    }
}

fn outcome_from(result: Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome::Success,
        Err(error) => match error.downcast_ref::<UserFacingError>() {
            Some(user) => Outcome::Refusal(user.0.clone()),
            None => Outcome::Error(format!("{error:#}")),
        },
    }
}

struct Selection {
    /// Index into the search results.
    index: usize,
    title: String,
    author: String,
}

async fn search_and_pick(
    backend: &Arc<dyn MediaBackend>,
    title: &str,
    select: Option<&str>,
) -> Result<(Vec<Box<dyn MediaItem>>, Selection)> {
    let items = backend.search(title).await?;
    if items.is_empty() {
        bail!("Search returned no results for {title:?}");
    }
    let index = match select {
        Some(needle) => {
            let needle = needle.to_lowercase();
            items
                .iter()
                .position(|item| item.to_dropdown().title.to_lowercase().contains(&needle))
                .with_context(|| format!("No search result matched selector {needle:?}"))?
        }
        None => 0,
    };
    let display = backend.display_info(&*items[index]);
    let author = display
        .subtitle
        .as_deref()
        .unwrap_or_default()
        .trim_start_matches("by ")
        .to_string();
    println!(
        "selected result #{index}: {:?} by {:?}",
        display.title, author
    );
    Ok((
        items,
        Selection {
            index,
            title: display.title,
            author,
        },
    ))
}

/// The identity fields of the lookup rows that match the selected title, for
/// the identity-drift observation (a `gr:` lookup resolving an `hc:` row).
async fn lookup_identities(api: &RawApi, term: &str, title: &str) -> Result<Vec<String>> {
    let encoded: String = url_encode(term);
    let rows = api.get(&format!("/book/lookup?term={encoded}")).await?;
    let wanted = normalize(title);
    let mut identities = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        if normalize(&field_str(row, "title")) != wanted {
            continue;
        }
        identities.push(format!(
            "foreignBookId={:?} goodreadsWorkId={:?} goodreadsBookId={:?} asin={:?} audibleASIN={:?}",
            field_str(row, "foreignBookId"),
            field_str(row, "goodreadsWorkId"),
            field_str(row, "goodreadsBookId"),
            field_str(row, "asin"),
            field_str(row, "audibleASIN"),
        ));
    }
    Ok(identities)
}

fn url_encode(term: &str) -> String {
    term.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Verify the checklist bullets for one completed (or refused) request.
async fn verify_request(
    api: &RawApi,
    format: Format,
    selection: &Selection,
    before: &Snapshot,
    expected_new_searches: usize,
    report: &mut Report,
) -> Result<()> {
    let after = take_snapshot(api).await?;

    // Locate the author by name.
    let wanted_author = normalize(&selection.author);
    let author = after
        .authors
        .iter()
        .find(|(_, gates)| normalize(&gates.name) == wanted_author);
    let Some((&author_id, gates)) = author else {
        report.check(
            expected_new_searches == 0,
            "author is absent locally (acceptable only for a refused request)",
        );
        return Ok(());
    };

    // Author gates: requested format open when a search was expected; the
    // unrequested format's gate unchanged from the before-snapshot.
    let (requested_gate, other_gate) = match format {
        Format::Ebook => (gates.ebook_future, gates.audiobook_future),
        Format::Audiobook => (gates.audiobook_future, gates.ebook_future),
    };
    if expected_new_searches > 0 {
        report.check(
            gates.monitored && requested_gate,
            &format!(
                "author {author_id} monitored with {} open",
                format.gate_flag()
            ),
        );
    }
    let other_before = before.authors.get(&author_id).map(|prior| match format {
        Format::Ebook => prior.audiobook_future,
        Format::Audiobook => prior.ebook_future,
    });
    report.check(
        other_gate == other_before.unwrap_or(false),
        &format!(
            "unrequested gate {} unchanged (now {other_gate})",
            format.other_gate_flag()
        ),
    );
    report.check(
        !gates.add_options_present,
        "author addOptions is spent (settle latch observed)",
    );

    // Commands first: the new BookSearch names the target row. The
    // canonical row's title can legitimately differ from the lookup's
    // (drift normalization plus edition-driven retitling, both observed
    // live), so the search command, not a title match, selects the target.
    let commands = api.get("/command").await?;
    let new_commands: Vec<&Value> = commands
        .as_array()
        .into_iter()
        .flatten()
        .filter(|command| field_id(command).is_some_and(|id| !before.command_ids.contains(&id)))
        .collect();
    let new_names: Vec<String> = new_commands.iter().map(|c| command_name(c)).collect();
    report.note(&format!("new commands since snapshot: {new_names:?}"));
    let manual_refresh = new_commands.iter().any(|command| {
        command_name(command).eq_ignore_ascii_case("RefreshAuthor")
            && field_str(command, "trigger").eq_ignore_ascii_case("manual")
    });
    report.check(
        !manual_refresh,
        "no manually-triggered RefreshAuthor was queued",
    );
    let new_searches: Vec<&Value> = new_commands
        .iter()
        .copied()
        .filter(|command| command_name(command).eq_ignore_ascii_case("BookSearch"))
        .collect();

    // Monitor drift across the author's rows.
    let rows_value = api.get(&format!("/book?authorId={author_id}")).await?;
    let rows: Vec<&Value> = rows_value.as_array().into_iter().flatten().collect();
    let mut changed: Vec<i64> = Vec::new();
    for row in &rows {
        let Some(book_id) = field_id(row) else {
            continue;
        };
        let now = (
            field_bool(row, "monitored"),
            field_bool(row, "ebookMonitored"),
            field_bool(row, "audiobookMonitored"),
        );
        let prior = before
            .books
            .get(&book_id)
            .copied()
            .unwrap_or((false, false, false));
        if now != prior {
            changed.push(book_id);
        }
    }

    if expected_new_searches == 0 {
        report.check(
            changed.is_empty(),
            &format!("no row's monitoring changed (changed: {changed:?})"),
        );
        report.check(new_searches.is_empty(), "no new BookSearch was queued");
        return Ok(());
    }

    report.check(
        new_searches.len() == expected_new_searches,
        &format!(
            "exactly {expected_new_searches} new BookSearch (saw {})",
            new_searches.len()
        ),
    );
    let search_targets: BTreeSet<i64> = new_searches
        .iter()
        .flat_map(|command| referenced_book_ids(command))
        .collect();
    report.check(
        search_targets.len() == 1,
        &format!("the new searches name exactly one row (saw {search_targets:?})"),
    );
    let Some(&book_id) = search_targets.iter().next() else {
        return Ok(());
    };
    let target = rows.iter().find(|row| field_id(row) == Some(book_id));
    report.check(
        target.is_some(),
        &format!("searched row {book_id} belongs to author {author_id}"),
    );
    let Some(target) = target else {
        return Ok(());
    };
    println!(
        "  target row {book_id}: title={:?} foreignBookId={:?} goodreadsWorkId={:?} asin={:?} audibleASIN={:?}",
        field_str(target, "title"),
        field_str(target, "foreignBookId"),
        field_str(target, "goodreadsWorkId"),
        field_str(target, "asin"),
        field_str(target, "audibleASIN"),
    );
    if normalize(&field_str(target, "title")) != normalize(&selection.title) {
        report.note("target title differs from the lookup display (drift-normalized row)");
    }
    report.check(
        field_str(target, "mediaType").eq_ignore_ascii_case(format.media_type())
            && field_bool(target, "monitored")
            && field_bool(target, format.monitored_flag()),
        &format!(
            "book {book_id} is a {} row, monitored with {} true",
            format.media_type(),
            format.monitored_flag()
        ),
    );
    let sibling_changes: Vec<i64> = changed.into_iter().filter(|id| *id != book_id).collect();
    report.check(
        sibling_changes.is_empty(),
        &format!("no sibling work's monitoring changed {sibling_changes:?}"),
    );

    // Editions: exactly one monitored, right readingFormatId, never 1.
    let editions = api.get(&format!("/edition?bookId={book_id}")).await?;
    let monitored: Vec<&Value> = editions
        .as_array()
        .into_iter()
        .flatten()
        .filter(|edition| field_bool(edition, "monitored"))
        .collect();
    let ids: Vec<Option<i64>> = monitored
        .iter()
        .map(|edition| edition.get("readingFormatId").and_then(Value::as_i64))
        .collect();
    report.check(
        monitored.len() == 1 && ids[0] == Some(format.reading_format_id()),
        &format!(
            "exactly one monitored edition with readingFormatId {} (saw {ids:?})",
            format.reading_format_id()
        ),
    );
    report.check(
        !ids.contains(&Some(1)),
        "no physical edition (readingFormatId 1) is monitored",
    );

    Ok(())
}

async fn run_request(
    backend: &Arc<dyn MediaBackend>,
    api: &RawApi,
    format: Format,
    title: &str,
    select: Option<&str>,
    expect_message: Option<&str>,
) -> Result<bool> {
    let mut report = Report::new();
    let before = take_snapshot(api).await?;
    let (mut items, selection) = search_and_pick(backend, title, select).await?;
    for identity in lookup_identities(api, title, &selection.title).await? {
        report.note(&format!("lookup identity: {identity}"));
    }

    // The confirmation screen runs the already-requested short-circuit; the
    // identity-drift probe surfaces here when the work is already local.
    let outcome = match backend.additional_details(&*items[selection.index]).await {
        Ok(details) => {
            let item = items.swap_remove(selection.index);
            outcome_from(backend.request(details, item, 1111).await)
        }
        Err(error) => outcome_from(Err(error)),
    };
    println!("outcome: {}", outcome.describe());

    let expected_new_searches = match (&outcome, expect_message) {
        (Outcome::Refusal(message), Some(expected)) => {
            report.check(
                message.contains(expected),
                &format!("refusal mentions {expected:?}"),
            );
            0
        }
        (_, Some(expected)) => {
            report.check(
                false,
                &format!("expected a refusal mentioning {expected:?}"),
            );
            0
        }
        (Outcome::Success, None) => 1,
        (Outcome::Refusal(_) | Outcome::Error(_), None) => {
            report.check(false, "request completed");
            0
        }
    };
    verify_request(
        api,
        format,
        &selection,
        &before,
        expected_new_searches,
        &mut report,
    )
    .await?;
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

async fn run_concurrent(
    backend: &Arc<dyn MediaBackend>,
    api: &RawApi,
    format: Format,
    title: &str,
    select: Option<&str>,
) -> Result<bool> {
    let mut report = Report::new();
    let before = take_snapshot(api).await?;
    let (mut items_one, selection) = search_and_pick(backend, title, select).await?;
    let (mut items_two, _) = search_and_pick(backend, title, select).await?;
    let item_one = items_one.swap_remove(selection.index);
    let item_two = items_two.swap_remove(selection.index);

    let (first, second) = tokio::join!(
        backend.request(Vec::new(), item_one, 1111),
        backend.request(Vec::new(), item_two, 2222),
    );
    let outcomes = [outcome_from(first), outcome_from(second)];
    for (which, outcome) in outcomes.iter().enumerate() {
        println!("concurrent requester {}: {}", which + 1, outcome.describe());
    }
    let successes = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Outcome::Success))
        .count();
    let already = outcomes
        .iter()
        .filter(|outcome| {
            matches!(outcome, Outcome::Refusal(message) if message.contains("already requested"))
        })
        .count();
    report.check(
        successes == 1 && already == 1,
        "one request completed, the concurrent duplicate was refused as already requested",
    );

    // Same-process retry immediately after a valid acknowledgement: the ack
    // cache must refuse it before any second search. The confirmation-stage
    // short-circuit is best effort (a drift-normalized work may have no
    // association or identity match yet), so the authoritative refusal may
    // come from request() itself - the checklist bullet is that no second
    // search gets queued.
    let (mut retry_items, retry_selection) = search_and_pick(backend, title, select).await?;
    let retried = match backend
        .additional_details(&*retry_items[retry_selection.index])
        .await
    {
        Ok(details) => {
            let item = retry_items.swap_remove(retry_selection.index);
            outcome_from(backend.request(details, item, 3333).await)
        }
        Err(error) => outcome_from(Err(error)),
    };
    println!("same-process retry: {}", retried.describe());
    report.check(
        matches!(&retried, Outcome::Refusal(message) if message.contains("already requested")),
        "immediate same-process retry is refused without a second search",
    );

    verify_request(api, format, &selection, &before, 1, &mut report).await?;
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

async fn run_reject_bundle(
    backend: &Arc<dyn MediaBackend>,
    api: &RawApi,
    title: &str,
    select: Option<&str>,
) -> Result<bool> {
    let mut report = Report::new();
    let before = take_snapshot(api).await?;
    let (mut items, selection) = search_and_pick(backend, title, select).await?;
    let outcome = match backend.additional_details(&*items[selection.index]).await {
        Ok(details) => {
            let item = items.swap_remove(selection.index);
            outcome_from(backend.request(details, item, 1111).await)
        }
        Err(error) => outcome_from(Err(error)),
    };
    println!("outcome: {}", outcome.describe());
    report.check(
        matches!(&outcome, Outcome::Refusal(message) if message.contains("multi-book collection")),
        "refused with the single-work limitation",
    );

    let after = take_snapshot(api).await?;
    report.check(
        after.authors.keys().collect::<Vec<_>>() == before.authors.keys().collect::<Vec<_>>(),
        "no author was added",
    );
    report.check(after.books == before.books, "no book monitoring changed");
    report.check(
        after.command_ids == before.command_ids,
        "no command was queued",
    );
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

/// Case 7 prep. Mirrors the provider's write shapes (`select_edition`'s
/// full-book PUT and the `/book/monitor` endpoint) against an existing
/// unmonitored row, then stops short of `POST /command` — the exact partial
/// state of a request that died between its monitor writes and its search.
async fn prepare_partial(
    api: &RawApi,
    format: Format,
    author_id: i64,
    select: Option<&str>,
) -> Result<bool> {
    let rows = api.get(&format!("/book?authorId={author_id}")).await?;
    let needle = select.map(str::to_lowercase);
    let mut prepared = None;
    for row in rows.as_array().into_iter().flatten() {
        let Some(book_id) = field_id(row) else {
            continue;
        };
        if field_bool(row, "monitored") {
            continue;
        }
        if !field_str(row, "mediaType").eq_ignore_ascii_case(format.media_type()) {
            continue;
        }
        if let Some(needle) = &needle
            && !field_str(row, "title").to_lowercase().contains(needle)
        {
            continue;
        }
        let editions = api.get(&format!("/edition?bookId={book_id}")).await?;
        let editions: Vec<Value> = editions.as_array().cloned().unwrap_or_default();
        let Some(chosen) = editions.iter().position(|edition| {
            edition.get("readingFormatId").and_then(Value::as_i64)
                == Some(format.reading_format_id())
        }) else {
            continue;
        };
        prepared = Some((book_id, row.clone(), editions, chosen));
        break;
    }
    let Some((book_id, row, editions, chosen)) = prepared else {
        bail!(
            "author {author_id} has no unmonitored {} row with a usable edition",
            format.media_type()
        );
    };

    // Author gate open, as the dead request would have left it.
    let mut author = api.get(&format!("/author/{author_id}")).await?;
    let object = author.as_object_mut().context("invalid author")?;
    object.insert(format.gate_flag().into(), Value::Bool(true));
    object.insert("monitored".into(), Value::Bool(true));
    let author_body = author.clone();
    api.send(
        reqwest::Method::PUT,
        &format!("/author/{author_id}"),
        &author_body,
    )
    .await?;

    // Edition pin: full-book PUT, anyEditionOk false, one monitored+manual.
    let mut body = row.clone();
    let object = body.as_object_mut().context("invalid book row")?;
    object.insert("anyEditionOk".into(), Value::Bool(false));
    let editions_payload: Vec<Value> = editions
        .iter()
        .enumerate()
        .map(|(index, edition)| {
            let mut edition = edition.clone();
            if let Some(fields) = edition.as_object_mut() {
                fields.insert("monitored".into(), Value::Bool(index == chosen));
                fields.insert("manualAdd".into(), Value::Bool(index == chosen));
            }
            edition
        })
        .collect();
    object.insert("editions".into(), Value::Array(editions_payload));
    api.send(reqwest::Method::PUT, &format!("/book/{book_id}"), &body)
        .await?;
    api.send(
        reqwest::Method::PUT,
        "/book/monitor",
        &json!({"bookIds": [book_id], "monitored": true}),
    )
    .await?;

    println!(
        "prepared partial state: book {book_id} ({:?}) monitored with a pinned {} edition and NO BookSearch",
        field_str(&row, "title"),
        format.media_type()
    );
    println!(
        "now run: chaptarr_canary request --format {} --title {:?} (fresh process)",
        format.media_type(),
        field_str(&row, "title")
    );
    Ok(true)
}

async fn run_settle_failure(
    backend: &Arc<dyn MediaBackend>,
    format: Format,
    title: &str,
    select: Option<&str>,
) -> Result<bool> {
    let mut report = Report::new();
    let (mut items, selection) = search_and_pick(backend, title, select).await?;
    println!(
        "requesting {:?} as {}; stop the container once the add is through",
        selection.title,
        format.media_type()
    );
    let item = items.swap_remove(selection.index);
    let outcome = outcome_from(backend.request(Vec::new(), item, 1111).await);
    println!("outcome: {}", outcome.describe());
    report.check(
        matches!(&outcome, Outcome::Refusal(message) if message.contains("NOT completed")),
        "request failed closed with the settle-deadline message",
    );
    println!("restart the container, then run verify-untouched for this author");
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

async fn run_verify_untouched(api: &RawApi, format: Format, author_needle: &str) -> Result<bool> {
    let mut report = Report::new();
    let needle = author_needle.to_lowercase();
    let authors = api.get("/author").await?;
    let author = authors
        .as_array()
        .into_iter()
        .flatten()
        .find(|author| {
            field_str(author, "authorName")
                .to_lowercase()
                .contains(&needle)
        })
        .cloned();
    let Some(author) = author else {
        report.note("author never materialized locally — nothing to verify");
        println!("verdict: PASS");
        return Ok(true);
    };
    let author_id = field_id(&author).context("author has no id")?;
    report.check(
        !field_bool(&author, format.gate_flag()),
        &format!("{} stayed closed", format.gate_flag()),
    );
    let rows = api.get(&format!("/book?authorId={author_id}")).await?;
    let monitored: Vec<i64> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| field_bool(row, "monitored"))
        .filter_map(field_id)
        .collect();
    report.check(
        monitored.is_empty(),
        &format!("no book row is monitored (saw {monitored:?})"),
    );
    let commands = api.get("/command").await?;
    let searches: Vec<i64> = commands
        .as_array()
        .into_iter()
        .flatten()
        .filter(|command| command_name(command).eq_ignore_ascii_case("BookSearch"))
        .filter_map(field_id)
        .collect();
    report.check(
        searches.is_empty(),
        &format!("no BookSearch was ever queued (saw {searches:?})"),
    );
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

/// Case 12: the one-time `/editions/wanted` probe from decision record 0001.
async fn probe_wanted(api: &RawApi, audiobook_book_id: i64, ebook_book_id: i64) -> Result<bool> {
    let mut report = Report::new();

    // An ebook row must be rejected outright.
    let ebook_editions = api.get(&format!("/edition?bookId={ebook_book_id}")).await?;
    let ebook_edition = ebook_editions
        .as_array()
        .into_iter()
        .flatten()
        .find_map(field_id)
        .context("ebook row has no editions")?;
    let (status, body) = api
        .send(
            reqwest::Method::POST,
            &format!("/book/{ebook_book_id}/editions/wanted"),
            &json!({"editionId": ebook_edition, "searchForNewBook": false}),
        )
        .await?;
    report.check(
        !(200..300).contains(&status),
        &format!("ebook row rejected (status {status}: {body})"),
    );

    // Audiobook row, author gate closed: the endpoint's own search must be
    // filtered out, and the author gates must not move.
    let audiobook_row = api.get(&format!("/book/{audiobook_book_id}")).await?;
    let author_id = audiobook_row
        .get("authorId")
        .and_then(Value::as_i64)
        .context("audiobook row has no authorId")?;
    let before_author = api.get(&format!("/author/{author_id}")).await?;
    let before = take_snapshot(api).await?;
    let editions = api
        .get(&format!("/edition?bookId={audiobook_book_id}"))
        .await?;
    let edition = editions
        .as_array()
        .into_iter()
        .flatten()
        .find(|edition| edition.get("readingFormatId").and_then(Value::as_i64) == Some(2))
        .and_then(field_id)
        .context("audiobook row has no audio edition")?;
    let (status, body) = api
        .send(
            reqwest::Method::POST,
            &format!("/book/{audiobook_book_id}/editions/wanted"),
            &json!({"editionId": edition, "searchForNewBook": true}),
        )
        .await?;
    report.note(&format!(
        "wanted-editions response: status {status}: {body}"
    ));

    let after_author = api.get(&format!("/author/{author_id}")).await?;
    for flag in ["monitored", "ebookMonitorFuture", "audiobookMonitorFuture"] {
        report.check(
            field_bool(&before_author, flag) == field_bool(&after_author, flag),
            &format!("author {flag} unchanged"),
        );
    }

    // The explicit manual command is never filtered.
    let (status, ack) = api
        .send(
            reqwest::Method::POST,
            "/command",
            &json!({"name": "BookSearch", "bookIds": [audiobook_book_id]}),
        )
        .await?;
    let ack_ok = (200..300).contains(&status)
        && command_name(&ack).eq_ignore_ascii_case("BookSearch")
        && matches!(
            field_str(&ack, "status").to_lowercase().as_str(),
            "queued" | "started" | "completed"
        );
    report.check(
        ack_ok,
        &format!("explicit manual BookSearch acknowledged (status {status})"),
    );

    let commands = api.get("/command").await?;
    let new_names: Vec<String> = commands
        .as_array()
        .into_iter()
        .flatten()
        .filter(|command| field_id(command).is_some_and(|id| !before.command_ids.contains(&id)))
        .map(|command| {
            format!(
                "{} (trigger {:?})",
                command_name(command),
                field_str(command, "trigger")
            )
        })
        .collect();
    report.note(&format!("commands queued by the probe: {new_names:?}"));
    println!("verdict: {}", if report.passed { "PASS" } else { "FAIL" });
    Ok(report.passed)
}

async fn dump_state(api: &RawApi, author_id: Option<i64>, book_id: Option<i64>) -> Result<bool> {
    if let Some(author_id) = author_id {
        let author = api.get(&format!("/author/{author_id}")).await?;
        println!(
            "author {author_id}: name={:?} monitored={} ebookMonitorFuture={} audiobookMonitorFuture={} addOptions={}",
            field_str(&author, "authorName"),
            field_bool(&author, "monitored"),
            field_bool(&author, "ebookMonitorFuture"),
            field_bool(&author, "audiobookMonitorFuture"),
            if author.get("addOptions").is_some() {
                "present"
            } else {
                "absent"
            },
        );
        let rows = api.get(&format!("/book?authorId={author_id}")).await?;
        for row in rows.as_array().into_iter().flatten() {
            println!(
                "  book {:?}: {:?} mediaType={:?} monitored={} ebookMonitored={} audiobookMonitored={} foreignBookId={:?} goodreadsWorkId={:?}",
                field_id(row),
                field_str(row, "title"),
                field_str(row, "mediaType"),
                field_bool(row, "monitored"),
                field_bool(row, "ebookMonitored"),
                field_bool(row, "audiobookMonitored"),
                field_str(row, "foreignBookId"),
                field_str(row, "goodreadsWorkId"),
            );
        }
    }
    if let Some(book_id) = book_id {
        let editions = api.get(&format!("/edition?bookId={book_id}")).await?;
        for edition in editions.as_array().into_iter().flatten() {
            println!(
                "  edition {:?}: readingFormatId={:?} monitored={} format={:?}",
                field_id(edition),
                edition.get("readingFormatId").and_then(Value::as_i64),
                field_bool(edition, "monitored"),
                field_str(edition, "format"),
            );
        }
    }
    let commands = api.get("/command").await?;
    let names: Vec<String> = commands
        .as_array()
        .into_iter()
        .flatten()
        .map(|command| {
            format!(
                "{:?} {} ({})",
                field_id(command),
                command_name(command),
                field_str(command, "status")
            )
        })
        .collect();
    println!("commands: {names:?}");
    Ok(true)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,doplarr=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let url = match cli.url {
        Some(url) => url,
        None => env_var("CHAPTARR_URL")?,
    };
    let api_key = env_var("CHAPTARR_API_KEY")?;
    let api = RawApi::new(&url, &api_key)?;

    let passed = match cli.case {
        Case::Check => {
            let connected = connect(&url, &api_key).await?;
            connected.print_preflight_report()?;
            true
        }
        Case::PreparePartial {
            format,
            author_id,
            select,
        } => prepare_partial(&api, format, author_id, select.as_deref()).await?,
        Case::VerifyUntouched { format, author } => {
            run_verify_untouched(&api, format, &author).await?
        }
        Case::ProbeWanted {
            audiobook_book_id,
            ebook_book_id,
        } => probe_wanted(&api, audiobook_book_id, ebook_book_id).await?,
        Case::State { author_id, book_id } => dump_state(&api, author_id, book_id).await?,
        Case::Request {
            format,
            title,
            select,
            expect_message,
        } => {
            let connected = connect(&url, &api_key).await?;
            let backend = backend_for(&connected.by_media, format)?;
            run_request(
                backend,
                &api,
                format,
                &title,
                select.as_deref(),
                expect_message.as_deref(),
            )
            .await?
        }
        Case::Concurrent {
            format,
            title,
            select,
        } => {
            let connected = connect(&url, &api_key).await?;
            let backend = backend_for(&connected.by_media, format)?;
            run_concurrent(backend, &api, format, &title, select.as_deref()).await?
        }
        Case::RejectBundle {
            format,
            title,
            select,
        } => {
            let connected = connect(&url, &api_key).await?;
            let backend = backend_for(&connected.by_media, format)?;
            run_reject_bundle(backend, &api, &title, select.as_deref()).await?
        }
        Case::SettleFailure {
            format,
            title,
            select,
        } => {
            let connected = connect(&url, &api_key).await?;
            let backend = backend_for(&connected.by_media, format)?;
            run_settle_failure(backend, format, &title, select.as_deref()).await?
        }
    };
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}

fn backend_for(
    by_media: &HashMap<String, Arc<dyn MediaBackend>>,
    format: Format,
) -> Result<&Arc<dyn MediaBackend>> {
    by_media
        .get(format.media())
        .with_context(|| format!("no {} backend connected", format.media()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const STATUS: &str = include_str!("../../tests/fixtures/chaptarr/system_status.json");
    const QUALITY: &str = include_str!("../../tests/fixtures/chaptarr/quality_profiles.json");
    const METADATA: &str = include_str!("../../tests/fixtures/chaptarr/metadata_profiles.json");
    const ROOTS: &str = include_str!("../../tests/fixtures/chaptarr/root_folders.json");

    /// Path-routed mock: enough of the API for startup resolution plus one
    /// verification read, over as many connections as the client opens.
    async fn mock_server() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    let Ok(read) = socket.read(&mut buffer).await else {
                        return;
                    };
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    counter.fetch_add(1, Ordering::SeqCst);
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    let body = if path.starts_with("/api/v1/system/status") {
                        STATUS
                    } else if path.starts_with("/api/v1/qualityprofile") {
                        QUALITY
                    } else if path.starts_with("/api/v1/metadataprofile") {
                        METADATA
                    } else if path.starts_with("/api/v1/rootfolder") {
                        ROOTS
                    } else {
                        // /author, /book, /command and friends: empty lists.
                        "[]"
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}"), hits)
    }

    #[tokio::test]
    async fn driver_constructs_both_backends_through_the_production_path() {
        let (url, hits) = mock_server().await;
        let connected = connect(&url, "canary-test-key").await.unwrap();
        assert!(connected.by_media.contains_key("book"));
        assert!(connected.by_media.contains_key("audiobook"));
        // Startup resolution actually talked to the instance.
        assert!(hits.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn snapshot_reads_are_get_only_and_key_stays_out_of_the_path() {
        let (url, _) = mock_server().await;
        let api = RawApi::new(&url, "canary-test-key").unwrap();
        let snapshot = take_snapshot(&api).await.unwrap();
        assert!(snapshot.authors.is_empty());
        assert!(snapshot.command_ids.is_empty());
        assert!(!api.base.contains("canary-test-key"));
    }

    #[test]
    fn url_encoding_covers_spaces_and_punctuation() {
        assert_eq!(url_encode("The Blazing World"), "The%20Blazing%20World");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn bundle_and_state_expectations_use_checklist_reading_format_ids() {
        assert_eq!(Format::Ebook.reading_format_id(), 3);
        assert_eq!(Format::Audiobook.reading_format_id(), 2);
        assert_eq!(Format::Ebook.media(), "book");
        assert_eq!(Format::Audiobook.media(), "audiobook");
    }
}
