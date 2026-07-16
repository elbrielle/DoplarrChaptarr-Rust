//! Chaptarr ebook and audiobook provider.
//!
//! Chaptarr is Readarr-shaped, but its per-format profile, root-folder, and
//! monitoring behavior is distinct enough that a small handwritten client is
//! safer than pretending it is a generic Readarr instance.

use super::*;
use crate::config::{BackendConfig, ChaptarrFormat};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{
    any::Any,
    cmp::Reverse,
    collections::{HashMap, VecDeque},
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, timeout},
};
use tracing::{debug, info, warn};

mod models;
mod selection;

use models::*;
use selection::*;

const API_PREFIX: &str = "/api/v1";
const OPEN_LIBRARY_SEARCH: &str = "https://openlibrary.org/search.json";
const RESOLVE_ATTEMPTS: usize = 20;
const RESOLVE_DEADLINE: Duration = Duration::from_secs(25);
/// Consecutive identical book-list samples (with no catalog command in
/// flight) required before a freshly imported catalog counts as settled.
const SETTLE_STABLE_SAMPLES: usize = 3;
const OPEN_LIBRARY_MIN_INTERVAL: Duration = Duration::from_secs(1);
const OPEN_LIBRARY_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const OPEN_LIBRARY_CACHE_CAPACITY: usize = 128;
const RECENT_SEARCH_ACK_TTL: Duration = Duration::from_secs(120);
const OPEN_LIBRARY_USER_AGENT: &str = concat!(
    "DoplarrChaptarr/",
    env!("CARGO_PKG_VERSION"),
    " (ebriellelucero@gmail.com)"
);

#[derive(Default)]
struct OpenLibraryState {
    last_request: Option<Instant>,
    cache: HashMap<String, (Instant, CoverMap)>,
    cache_order: VecDeque<String>,
}

static OPEN_LIBRARY_STATE: OnceLock<Mutex<OpenLibraryState>> = OnceLock::new();
static CHAPTARR_MUTATION_LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
static CHAPTARR_RECENT_SEARCHES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn open_library_state() -> &'static Mutex<OpenLibraryState> {
    OPEN_LIBRARY_STATE.get_or_init(|| Mutex::new(OpenLibraryState::default()))
}

fn mutation_locks() -> &'static Mutex<HashMap<String, Weak<Mutex<()>>>> {
    CHAPTARR_MUTATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn recent_searches() -> &'static Mutex<HashMap<String, Instant>> {
    CHAPTARR_RECENT_SEARCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct Chaptarr {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    server_version: String,
    format: ChaptarrFormat,
    openlibrary_covers: bool,
    settings: ResolvedSettings,
    settle: SettlePacing,
}

/// Pacing for the new-author catalog-settle wait. A 400+ book author takes
/// real time to import; any monitor or edition work done before the catalog
/// settles can be silently reverted by the tail of that import.
#[derive(Debug, Clone)]
struct SettlePacing {
    interval: Duration,
    deadline: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogFingerprint {
    books: Vec<BookRowFingerprint>,
    targets: Vec<TargetFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TargetFingerprint {
    book_id: i64,
    complete: bool,
    release_date: String,
    foreign_edition_id: String,
    editions: Vec<EditionFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EditionFingerprint {
    id: String,
    foreign_edition_id: String,
    format: String,
    is_ebook: Option<bool>,
    language: String,
    title: String,
    isbn13: String,
    asin: String,
}

impl Default for SettlePacing {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            deadline: Duration::from_secs(240),
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedSettings {
    ebook_root: String,
    audiobook_root: String,
    ebook_quality: i32,
    audiobook_quality: i32,
    ebook_metadata: i32,
    audiobook_metadata: i32,
}

#[derive(Debug, Clone)]
struct ChaptarrItem {
    book: BookShape,
    display_title: String,
    cover: Option<String>,
    existing_book_id: Option<i64>,
}

impl Chaptarr {
    pub async fn connect(backend: BackendConfig, client: reqwest::Client) -> Result<Self> {
        let BackendConfig::Chaptarr {
            url,
            api_key,
            format,
            ebook_rootfolder,
            audiobook_rootfolder,
            ebook_quality_profile,
            audiobook_quality_profile,
            ebook_metadata_profile,
            audiobook_metadata_profile,
            openlibrary_covers,
        } = backend
        else {
            bail!("Configured backend not for Chaptarr");
        };

        let url = url.trim_end_matches('/');
        let base_url = if url.ends_with(API_PREFIX) {
            url.to_string()
        } else {
            format!("{url}{API_PREFIX}")
        };
        let mut backend = Self {
            client,
            base_url,
            api_key,
            server_version: String::new(),
            format,
            openlibrary_covers: openlibrary_covers.unwrap_or(true),
            settle: SettlePacing::default(),
            settings: ResolvedSettings {
                ebook_root: String::new(),
                audiobook_root: String::new(),
                ebook_quality: 0,
                audiobook_quality: 0,
                ebook_metadata: 0,
                audiobook_metadata: 0,
            },
        };

        info!(format = ?format, "Connecting to Chaptarr");
        debug!(url = %backend.base_url, "Chaptarr endpoint");
        let (status, roots, quality, metadata) = tokio::try_join!(
            backend.get::<SystemStatus>("/system/status", &[]),
            backend.get::<Vec<RootFolder>>("/rootfolder", &[]),
            backend.get::<Vec<Profile>>("/qualityprofile", &[]),
            backend.get::<Vec<Profile>>("/metadataprofile", &[]),
        )?;
        validate_system_status(&status)?;
        backend.server_version = status.version;
        backend.settings = ResolvedSettings {
            ebook_root: resolve_root(&roots, ChaptarrFormat::Ebook, ebook_rootfolder.as_deref())?,
            audiobook_root: resolve_root(
                &roots,
                ChaptarrFormat::Audiobook,
                audiobook_rootfolder.as_deref(),
            )?,
            ebook_quality: resolve_profile(
                &quality,
                ChaptarrFormat::Ebook,
                false,
                ebook_quality_profile.as_deref(),
            )?,
            audiobook_quality: resolve_profile(
                &quality,
                ChaptarrFormat::Audiobook,
                false,
                audiobook_quality_profile.as_deref(),
            )?,
            ebook_metadata: resolve_profile(
                &metadata,
                ChaptarrFormat::Ebook,
                true,
                ebook_metadata_profile.as_deref(),
            )?,
            audiobook_metadata: resolve_profile(
                &metadata,
                ChaptarrFormat::Audiobook,
                true,
                audiobook_metadata_profile.as_deref(),
            )?,
        };
        Ok(backend)
    }

    pub(crate) fn server_version(&self) -> &str {
        &self.server_version
    }

    pub(crate) fn server_version_is_tested(&self) -> bool {
        version_is_tested(&self.server_version)
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, endpoint))
            .header("X-Api-Key", &self.api_key)
            .query(query)
            .send()
            .await
            .with_context(|| format!("Could not reach Chaptarr endpoint {endpoint}"))?;
        self.decode(response, endpoint).await
    }

    async fn send_json(&self, method: Method, endpoint: &str, body: &Value) -> Result<Value> {
        let bytes = serde_json::to_vec(body)?;
        let response = self
            .client
            .request(method, format!("{}{}", self.base_url, endpoint))
            .header("X-Api-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("Could not reach Chaptarr endpoint {endpoint}"))?;
        self.decode_value(response, endpoint).await
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
        endpoint: &str,
    ) -> Result<T> {
        let value = self.decode_value(response, endpoint).await?;
        serde_json::from_value(value)
            .with_context(|| format!("Chaptarr returned an unexpected response from {endpoint}"))
    }

    async fn decode_value(&self, response: reqwest::Response, endpoint: &str) -> Result<Value> {
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).trim().to_string());
            warn!(
                %status,
                endpoint,
                response_detail = %detail,
                "Chaptarr API request failed"
            );
            let message = match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    "Chaptarr rejected its API key.".to_string()
                }
                StatusCode::NOT_FOUND => {
                    format!("Chaptarr does not support the required endpoint {endpoint}.")
                }
                _ => {
                    "Chaptarr could not complete that request. Check the service logs for details."
                        .to_string()
                }
            };
            bail!(UserFacingError(message));
        }
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("Chaptarr returned invalid JSON from {endpoint}"))
        }
    }

    async fn lookup(&self, term: &str) -> Result<Vec<Value>> {
        self.get("/book/lookup", &[("term", term.to_string())])
            .await
    }

    async fn open_library_covers(&self, term: &str) -> CoverMap {
        let cache_key = normalize(term);
        let mut state = open_library_state().lock().await;
        if let Some((created, covers)) = state.cache.get(&cache_key)
            && created.elapsed() < OPEN_LIBRARY_CACHE_TTL
        {
            return covers.clone();
        }
        state.cache.remove(&cache_key);
        state.cache_order.retain(|key| key != &cache_key);

        if let Some(last_request) = state.last_request {
            let elapsed = last_request.elapsed();
            if elapsed < OPEN_LIBRARY_MIN_INTERVAL {
                sleep(OPEN_LIBRARY_MIN_INTERVAL - elapsed).await;
            }
        }
        state.last_request = Some(Instant::now());

        let fetch = async {
            let response = self
                .client
                .get(OPEN_LIBRARY_SEARCH)
                .header(reqwest::header::USER_AGENT, OPEN_LIBRARY_USER_AGENT)
                .query(&[
                    ("q", term),
                    ("fields", "title,author_name,cover_i"),
                    ("limit", "20"),
                ])
                .send()
                .await?
                .error_for_status()?;
            let bytes = response.bytes().await?;
            serde_json::from_slice::<OpenLibraryResponse>(&bytes).map_err(anyhow::Error::from)
        };
        let Ok(Ok(result)) = timeout(Duration::from_secs(5), fetch).await else {
            debug!("Open Library cover enrichment was unavailable");
            return HashMap::new();
        };
        let covers = open_library_cover_map(result);

        if !state.cache.contains_key(&cache_key) {
            while state.cache_order.len() >= OPEN_LIBRARY_CACHE_CAPACITY {
                if let Some(oldest) = state.cache_order.pop_front() {
                    state.cache.remove(&oldest);
                }
            }
            state.cache_order.push_back(cache_key.clone());
        }
        state
            .cache
            .insert(cache_key, (Instant::now(), covers.clone()));
        covers
    }

    async fn authors(&self) -> Result<Vec<Value>> {
        self.get("/author", &[]).await
    }

    async fn find_author(&self, item: &ChaptarrItem) -> Result<Option<Value>> {
        let authors = self.authors().await?;
        let fid = item.book.author.foreign_author_id.trim();
        if !fid.is_empty() {
            let matches: Vec<_> = authors
                .iter()
                .filter(|a| string_at(a, "foreignAuthorId") == fid)
                .cloned()
                .collect();
            if matches.len() == 1 {
                return Ok(matches.into_iter().next());
            } else if matches.len() > 1 {
                bail!(UserFacingError(
                    "Chaptarr has multiple local authors with the same external identity. Resolve that duplicate before requesting this book."
                        .into()
                ));
            }
        }
        let wanted = normalize(&item.book.author.author_name);
        let matches: Vec<_> = authors
            .into_iter()
            .filter(|a| !wanted.is_empty() && normalize(string_at(a, "authorName")) == wanted)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => bail!(UserFacingError(
                "Chaptarr has multiple local authors with this name. Select a result with a stable external author identity or resolve the duplicate first."
                    .into()
            )),
        }
    }

    async fn books_for_author(&self, author_id: i64) -> Result<Vec<Value>> {
        self.get("/book", &[("authorId", author_id.to_string())])
            .await
    }

    async fn get_book(&self, id: i64) -> Result<Value> {
        self.get(&format!("/book/{id}"), &[]).await
    }

    /// The book resource always reports `"editions": []`; this endpoint is the
    /// only source of edition truth and every edition read must go through it.
    async fn editions_for_book(&self, book_id: i64) -> Result<Vec<Value>> {
        self.get("/edition", &[("bookId", book_id.to_string())])
            .await
    }

    async fn commands(&self) -> Result<Vec<Value>> {
        self.get("/command", &[]).await
    }

    /// Locate the local author, the preferred matching book row, and every
    /// book row of that author. The full row set matters because duplicate
    /// import pockets can hold the requested work's real state on a twin row.
    async fn locate_existing(
        &self,
        item: &ChaptarrItem,
    ) -> Result<(Option<Value>, Option<Value>, Vec<Value>)> {
        if let Some(id) = item.existing_book_id {
            let book = self.get_book(id).await?;
            if local_row_matches_item(&book, self.format, &item.book) {
                let author_id = positive_id(book.get("authorId"));
                let author = if let Some(id) = author_id {
                    Some(self.get(&format!("/author/{id}"), &[]).await?)
                } else {
                    self.find_author(item).await?
                };
                let rows = if let Some(author_id) =
                    author.as_ref().and_then(|a| positive_id(a.get("id")))
                {
                    self.books_for_author(author_id).await?
                } else {
                    vec![book.clone()]
                };
                return Ok((author, Some(book), rows));
            }
            warn!(
                book_id = id,
                format = ?self.format,
                "Ignoring lookup local-book id for the wrong format"
            );
        }
        let author = self.find_author(item).await?;
        let Some(author_id) = author.as_ref().and_then(|a| positive_id(a.get("id"))) else {
            return Ok((None, None, Vec::new()));
        };
        let rows = self.books_for_author(author_id).await?;
        let book = preferred_book(&rows, self.format, &item.book);
        Ok((author, book, rows))
    }

    async fn poll_target(&self, author_id: i64, selected: &BookShape) -> Result<Option<Value>> {
        let poll = async {
            let mut last = None;
            for attempt in 0..RESOLVE_ATTEMPTS {
                let rows = self.books_for_author(author_id).await?;
                last = preferred_book(&rows, self.format, selected);
                if last.as_ref().is_some_and(book_complete) {
                    return Ok(last);
                }
                if attempt + 1 < RESOLVE_ATTEMPTS {
                    sleep(Duration::from_secs(1)).await;
                }
            }
            Ok(last)
        };
        match timeout(RESOLVE_DEADLINE, poll).await {
            Ok(result) => result,
            Err(_) => {
                warn!(author_id, "Chaptarr metadata polling hit its hard deadline");
                Ok(None)
            }
        }
    }

    async fn catalog_fingerprint(
        &self,
        rows: &[Value],
        selected: &BookShape,
        require_target: bool,
    ) -> Result<Option<CatalogFingerprint>> {
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| local_row_matches_item(row, self.format, selected))
            .collect();
        if require_target && !matching.iter().any(|target| book_complete(target)) {
            return Ok(None);
        }
        let mut targets = Vec::new();
        for row in matching {
            let Some(book_id) = positive_id(row.get("id")) else {
                continue;
            };
            let mut editions = self
                .editions_for_book(book_id)
                .await?
                .iter()
                .map(|edition| EditionFingerprint {
                    id: edition.get("id").map(Value::to_string).unwrap_or_default(),
                    foreign_edition_id: string_at(edition, "foreignEditionId").to_string(),
                    format: string_at(edition, "format").to_ascii_lowercase(),
                    is_ebook: edition.get("isEbook").and_then(Value::as_bool),
                    language: string_at(edition, "language").to_ascii_lowercase(),
                    title: string_at(edition, "title").to_string(),
                    isbn13: string_at(edition, "isbn13").to_string(),
                    asin: string_at(edition, "asin").to_string(),
                })
                .collect::<Vec<_>>();
            editions.sort();
            targets.push(TargetFingerprint {
                book_id,
                complete: book_complete(row),
                release_date: string_at(row, "releaseDate").to_string(),
                foreign_edition_id: string_at(row, "foreignEditionId").to_string(),
                editions,
            });
        }
        targets.sort();
        Ok(Some(CatalogFingerprint {
            books: book_list_fingerprint(rows),
            targets,
        }))
    }

    /// Block until an author's catalog stops moving: no queued/running
    /// command that could touch this author, and an unchanged book list plus
    /// target-edition snapshot across consecutive samples. Monitoring or
    /// edition selection performed before this point can be silently reverted
    /// by the tail of the import, which is exactly the failure that strands a
    /// request.
    async fn wait_for_catalog_settle(
        &self,
        author_id: i64,
        author_name: &str,
        selected: &BookShape,
        require_target: bool,
    ) -> Result<()> {
        let started = Instant::now();
        let deadline = started + self.settle.deadline;
        let mut previous_fingerprint = None;
        let mut stable_samples = 0_usize;
        loop {
            let commands = match self.commands().await {
                Ok(commands) => commands,
                Err(error) => {
                    warn!(author_id, %error, "Could not poll Chaptarr commands during settle");
                    // Unknown command state is never equivalent to idle. A
                    // failed poll invalidates the entire quiet window.
                    stable_samples = 0;
                    previous_fingerprint = None;
                    if Instant::now() + self.settle.interval > deadline {
                        break;
                    }
                    sleep(self.settle.interval).await;
                    continue;
                }
            };
            let busy = catalog_command_active(&commands, author_id);
            if busy {
                stable_samples = 0;
                previous_fingerprint = None;
            } else {
                let rows = self.books_for_author(author_id).await?;
                let fingerprint = if rows.is_empty() && require_target {
                    None
                } else {
                    self.catalog_fingerprint(&rows, selected, require_target)
                        .await?
                };
                if let Some(fingerprint) = fingerprint {
                    if previous_fingerprint.as_ref() == Some(&fingerprint) {
                        stable_samples += 1;
                    } else {
                        stable_samples = 1;
                    }
                    previous_fingerprint = Some(fingerprint);
                    if stable_samples >= SETTLE_STABLE_SAMPLES {
                        info!(
                            author_id,
                            books = rows.len(),
                            elapsed_secs = started.elapsed().as_secs(),
                            "Chaptarr catalog settled"
                        );
                        return Ok(());
                    }
                } else {
                    stable_samples = 0;
                    previous_fingerprint = None;
                }
            }
            if Instant::now() + self.settle.interval > deadline {
                break;
            }
            sleep(self.settle.interval).await;
        }
        warn!(
            author_id,
            "Chaptarr catalog never settled before the deadline"
        );
        bail!(UserFacingError(format!(
            "Chaptarr is still importing {author_name}'s catalog, so this request was NOT completed. Nothing was monitored or searched yet - please request it again in a few minutes."
        )));
    }

    /// Resolve the one local row to monitor, edition-aware. Duplicate import
    /// pockets for the same work are disambiguated by which row actually has
    /// usable requested-format editions (read from `/edition`, never from the
    /// book resource, which always reports an empty editions array).
    async fn resolve_pocket(
        &self,
        author_id: i64,
        selected: &BookShape,
    ) -> Result<Option<(Value, Vec<Value>)>> {
        let rows = self.books_for_author(author_id).await?;
        let mut candidates = Vec::new();
        for row in rows {
            if !local_row_matches_item(&row, self.format, selected) {
                continue;
            }
            let editions = match positive_id(row.get("id")) {
                Some(id) => self.editions_for_book(id).await?,
                None => Vec::new(),
            };
            candidates.push((row, editions));
        }
        if candidates.len() > 1 {
            warn!(
                author_id,
                pockets = candidates.len(),
                foreign_book_id = %selected.foreign_book_id,
                "Duplicate Chaptarr pockets for one work; selecting by usable editions"
            );
        }
        let Some(index) = preferred_pocket(&candidates, self.format, selected) else {
            return Ok(None);
        };
        Ok(Some(candidates.swap_remove(index)))
    }

    /// Mirror the Chaptarr UI's manual edition pick: a full-book PUT carrying
    /// the complete book body, `anyEditionOk = false`, and the full editions
    /// array with exactly one edition monitored and manually added. This is
    /// the only edition write Chaptarr persists correctly.
    async fn select_edition(&self, book: &Value, editions: &[Value], chosen: usize) -> Result<()> {
        let book_id = positive_id(book.get("id")).context("Resolved Chaptarr book has no id")?;
        let mut body = book.clone();
        let object = body
            .as_object_mut()
            .context("Chaptarr returned an invalid book")?;
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
        self.send_json(Method::PUT, &format!("/book/{book_id}"), &body)
            .await?;
        Ok(())
    }

    /// The read-back whose absence made the original failure silent: confirm
    /// the book row is monitored for the requested format AND that exactly one
    /// requested-format edition - the one we chose - is monitored, before any
    /// search is queued or any success is reported.
    async fn verify_request_ready(
        &self,
        book_id: i64,
        item: &ChaptarrItem,
        chosen_edition: &Value,
    ) -> Result<()> {
        let verified = self.get_book(book_id).await?;
        if !local_row_matches_item(&verified, self.format, &item.book)
            || !format_is_monitored(&verified, self.format)
        {
            bail!(UserFacingError(
                "Chaptarr did not keep the requested format monitored, so no search was queued."
                    .into()
            ));
        }
        let editions = self.editions_for_book(book_id).await?;
        let chosen = parse_edition(chosen_edition)
            .context("The selected Chaptarr edition could not be re-read")?;
        let confirmed = sole_monitored_edition(&editions, self.format)
            .is_some_and(|edition| same_edition(&edition, &chosen));
        if !confirmed {
            bail!(UserFacingError(format!(
                "Chaptarr did not keep exactly one {} edition of {} monitored, so no search was queued. The book needs attention in Chaptarr.",
                format_name(self.format),
                item.book.title
            )));
        }
        Ok(())
    }

    async fn enable_author_format(&self, author_id: i64) -> Result<()> {
        let endpoint = format!("/author/{author_id}");
        let mut author: Value = self.get(&endpoint, &[]).await?;
        let flag = match self.format {
            ChaptarrFormat::Ebook => "ebookMonitorFuture",
            ChaptarrFormat::Audiobook => "audiobookMonitorFuture",
        };
        if author.get(flag).and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let object = author
            .as_object_mut()
            .context("Chaptarr returned an invalid author")?;
        object.insert(flag.into(), Value::Bool(true));
        object.insert("monitored".into(), Value::Bool(true));
        self.send_json(Method::PUT, &endpoint, &author).await?;
        let verified: Value = self.get(&endpoint, &[]).await?;
        if verified.get(flag).and_then(Value::as_bool) != Some(true) {
            bail!(UserFacingError(format!(
                "Chaptarr did not enable {} monitoring for this author, so no search was queued.",
                format_name(self.format)
            )));
        }
        Ok(())
    }

    fn new_author_body(&self, item: &ChaptarrItem) -> Value {
        let s = &self.settings;
        let chosen_root = match self.format {
            ChaptarrFormat::Ebook => &s.ebook_root,
            ChaptarrFormat::Audiobook => &s.audiobook_root,
        };
        json!({
            "title": item.book.title,
            "foreignBookId": item.book.foreign_book_id,
            "mediaType": format_name(self.format),
            "monitored": false,
            "ebookMonitored": false,
            "audiobookMonitored": false,
            "rootFolderPath": chosen_root,
            "ebookQualityProfileId": s.ebook_quality,
            "audiobookQualityProfileId": s.audiobook_quality,
            "ebookMetadataProfileId": s.ebook_metadata,
            "audiobookMetadataProfileId": s.audiobook_metadata,
            "author": {
                "authorName": item.book.author.author_name,
                "foreignAuthorId": item.book.author.foreign_author_id,
                "ebookQualityProfileId": s.ebook_quality,
                "audiobookQualityProfileId": s.audiobook_quality,
                "ebookMetadataProfileId": s.ebook_metadata,
                "audiobookMetadataProfileId": s.audiobook_metadata,
                "rootFolderPath": chosen_root,
                "ebookRootFolderPath": s.ebook_root,
                "audiobookRootFolderPath": s.audiobook_root,
                "ebookMonitorFuture": self.format == ChaptarrFormat::Ebook,
                "audiobookMonitorFuture": self.format == ChaptarrFormat::Audiobook,
                "monitored": true,
                "monitorNewItems": "none",
                "addOptions": {"monitor": "none", "searchForMissingBooks": false}
            },
            "addOptions": {"searchForNewBook": false}
        })
    }

    fn existing_author_book_body(&self, item: &ChaptarrItem, author: &Value) -> Result<Value> {
        let author_id = positive_id(author.get("id")).context("Local author has no id")?;
        let expected_ebook = self.format == ChaptarrFormat::Ebook;
        let selected_editions: Vec<Value> = item
            .book
            .editions
            .iter()
            .filter(|edition| edition_usable(edition, self.format))
            .map(|edition| {
                json!({
                    "title": if edition.title.is_empty() { &item.book.title } else { &edition.title },
                    "foreignEditionId": null_if_empty(&edition.foreign_edition_id),
                    "format": format_name(self.format),
                    "isEbook": expected_ebook,
                    "isbn13": edition.isbn13,
                    "monitored": false,
                    "manualAdd": false
                })
            })
            .collect();
        // Lookup projections may omit authoritative format. Never relabel or
        // carry such a projection (or a physical edition) into a write. A
        // neutral requested-format placeholder lets Chaptarr resolve its own
        // authoritative `/edition` rows, which are settled and re-read before
        // any edition is selected.
        let editions = if selected_editions.is_empty() {
            vec![json!({
                "title": item.book.title,
                "format": format_name(self.format),
                "isEbook": expected_ebook,
                "monitored": false,
                "manualAdd": false
            })]
        } else {
            selected_editions
        };
        let s = &self.settings;
        let chosen_root = match self.format {
            ChaptarrFormat::Ebook => &s.ebook_root,
            ChaptarrFormat::Audiobook => &s.audiobook_root,
        };
        Ok(json!({
            "id": 0,
            "localBookId": null,
            "authorId": author_id,
            "title": item.book.title,
            "foreignBookId": item.book.foreign_book_id,
            "mediaType": format_name(self.format),
            "monitored": false,
            "ebookMonitored": false,
            "audiobookMonitored": false,
            "rootFolderPath": chosen_root,
            "ebookQualityProfileId": s.ebook_quality,
            "audiobookQualityProfileId": s.audiobook_quality,
            "ebookMetadataProfileId": s.ebook_metadata,
            "audiobookMetadataProfileId": s.audiobook_metadata,
            "editions": editions,
            "author": {
                "id": author_id,
                "authorName": string_at(author, "authorName"),
                "foreignAuthorId": string_at(author, "foreignAuthorId"),
                "monitored": author.get("monitored").and_then(Value::as_bool).unwrap_or(true),
                "ebookMonitorFuture": author.get("ebookMonitorFuture").and_then(Value::as_bool).unwrap_or(false),
                "audiobookMonitorFuture": author.get("audiobookMonitorFuture").and_then(Value::as_bool).unwrap_or(false),
                "ebookQualityProfileId": s.ebook_quality,
                "audiobookQualityProfileId": s.audiobook_quality,
                "ebookMetadataProfileId": s.ebook_metadata,
                "audiobookMetadataProfileId": s.audiobook_metadata,
                "ebookRootFolderPath": s.ebook_root,
                "audiobookRootFolderPath": s.audiobook_root
            },
            "addOptions": {"searchForNewBook": false}
        }))
    }

    async fn already_requested(&self, item: &ChaptarrItem) -> Result<Option<FormatState>> {
        let (_, _, rows) = self.locate_existing(item).await?;
        let state = self.request_state_across(&rows, item).await?;
        Ok((state != FormatState::Missing).then_some(state))
    }

    /// Monitoring is only a durable configuration flag; it does not prove a
    /// search was queued. Treat a no-file row as genuinely in flight only when
    /// Chaptarr reports a grab or an active BookSearch for that exact pocket.
    /// This lets a later Discord retry repair edition/monitor/search failures
    /// without allowing two concurrent requests through the mutation lock.
    async fn request_state_across(
        &self,
        rows: &[Value],
        item: &ChaptarrItem,
    ) -> Result<FormatState> {
        let state = format_state_across(rows, self.format, &item.book);
        if state == FormatState::Available {
            return Ok(FormatState::Available);
        }
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| local_row_matches_item(row, self.format, &item.book))
            .collect();
        if matching
            .iter()
            .filter_map(|row| serde_json::from_value::<BookShape>((*row).clone()).ok())
            .any(|book| book.grabbed)
        {
            return Ok(FormatState::Processing);
        }
        let book_ids: Vec<_> = matching
            .iter()
            .filter_map(|row| positive_id(row.get("id")))
            .collect();
        // A strict monitored state needs command evidence before it can block
        // a repair. A legacy/partial row can also have a real search in flight,
        // so probe whenever matching local IDs exist, even if monitoring is
        // incomplete.
        if book_ids.is_empty() {
            return Ok(FormatState::Missing);
        }
        if self.recent_search_acknowledged(&book_ids).await {
            return Ok(FormatState::Processing);
        }
        let commands = match self.commands().await {
            Ok(commands) => commands,
            Err(error) => {
                warn!(%error, "Could not verify whether the requested book is already being searched");
                bail!(UserFacingError(
                    "Chaptarr's search status could not be verified, so no duplicate search was started. Please try again in a moment."
                        .into()
                ));
            }
        };
        if book_search_command_active(&commands, &book_ids) {
            Ok(FormatState::Processing)
        } else {
            // A monitored row with no file, grab, or matching search is a
            // recoverable partial request, not proof of successful work.
            Ok(FormatState::Missing)
        }
    }

    fn recent_search_key(&self, book_id: i64) -> String {
        format!("{}:{}:{book_id}", self.base_url, format_name(self.format))
    }

    async fn recent_search_acknowledged(&self, book_ids: &[i64]) -> bool {
        let now = Instant::now();
        let mut searches = recent_searches().lock().await;
        searches
            .retain(|_, acknowledged| now.duration_since(*acknowledged) < RECENT_SEARCH_ACK_TTL);
        book_ids
            .iter()
            .any(|id| searches.contains_key(&self.recent_search_key(*id)))
    }

    async fn record_search_acknowledgement(&self, book_id: i64) {
        let now = Instant::now();
        let mut searches = recent_searches().lock().await;
        searches
            .retain(|_, acknowledged| now.duration_since(*acknowledged) < RECENT_SEARCH_ACK_TTL);
        searches.insert(self.recent_search_key(book_id), now);
    }
}

fn command_references_book(command: &Value, book_ids: &[i64]) -> bool {
    [command.get("body"), Some(command)]
        .into_iter()
        .flatten()
        .any(|scope| {
            positive_id(scope.get("bookId")).is_some_and(|id| book_ids.contains(&id))
                || scope
                    .get("bookIds")
                    .and_then(Value::as_array)
                    .is_some_and(|ids| {
                        ids.iter().any(|id| {
                            positive_id(Some(id)).is_some_and(|id| book_ids.contains(&id))
                        })
                    })
        })
}

fn command_name(command: &Value) -> &str {
    let name = string_at(command, "name");
    if name.is_empty() {
        string_at(command, "commandName")
    } else {
        name
    }
}

fn book_search_command_active(commands: &[Value], book_ids: &[i64]) -> bool {
    !book_ids.is_empty()
        && commands.iter().any(|command| {
            let status = string_at(command, "status").to_ascii_lowercase();
            matches!(status.as_str(), "queued" | "started")
                && command_name(command).eq_ignore_ascii_case("BookSearch")
                && command_references_book(command, book_ids)
        })
}

fn valid_book_search_acknowledgement(response: &Value) -> bool {
    let status = string_at(response, "status").to_ascii_lowercase();
    positive_id(response.get("id")).is_some()
        && command_name(response).eq_ignore_ascii_case("BookSearch")
        && matches!(status.as_str(), "queued" | "started" | "completed")
}

impl MediaItem for ChaptarrItem {
    fn to_dropdown(&self) -> DropdownOption {
        DropdownOption {
            title: truncate_chars(&self.display_title, 100),
            description: self
                .book
                .release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .map(str::to_owned),
            id: self
                .existing_book_id
                .map(|id| SelectableId::Integer(id as i32)),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[async_trait]
impl MediaBackend for Chaptarr {
    async fn search(&self, term: &str) -> Result<Vec<Box<dyn MediaItem>>> {
        info!(format = ?self.format, "Searching Chaptarr for book");
        let lookup = self.lookup(term).await?;
        let raw_count = lookup.len();
        let mut items = Vec::new();
        let mut item_affinities = Vec::new();
        let mut seen = HashMap::new();
        let mut malformed_count = 0;
        let mut incomplete_identity_count = 0;
        let mut junk_count = 0;
        let mut duplicate_count = 0;
        let mut other_format_projection_count = 0;
        for raw in lookup {
            let Ok(book) = serde_json::from_value::<BookShape>(raw.clone()) else {
                malformed_count += 1;
                warn!("Skipping malformed Chaptarr lookup result");
                continue;
            };
            if !book_identity_complete(&book) {
                incomplete_identity_count += 1;
                debug!(title = %book.title, "Skipping incomplete Chaptarr lookup result");
                continue;
            }
            if junk_title(&book.title) {
                junk_count += 1;
                debug!(title = %book.title, "Skipping Chaptarr lookup result marked as non-work material");
                continue;
            }
            // Chaptarr's lookup endpoint projects a work through one metadata
            // format. A row labelled `audiobook` can still be the correct work
            // for an ebook request (and vice versa), so this is a preference
            // for duplicate projections, never a search-stage exclusion.
            let format_affinity = search_format_affinity(&book, self.format);
            if format_affinity == 0 {
                other_format_projection_count += 1;
            }
            let key = if book.foreign_book_id.is_empty() {
                format!(
                    "{}|{}",
                    normalize(&book.title),
                    normalize(&book.author.author_name)
                )
            } else {
                book.foreign_book_id.clone()
            };
            let author = book.author.author_name.trim();
            let display_title = if author.is_empty() {
                book.title.clone()
            } else {
                format!("{} — {author}", book.title)
            };
            let cover = absolute_cover(&book, self.format)
                .or_else(|| public_identifier_cover(&book, self.format));
            let local = match self.format {
                ChaptarrFormat::Ebook => &book.local_ebook_books,
                ChaptarrFormat::Audiobook => &book.local_audiobook_books,
            };
            let existing_book_id = local.iter().find_map(|b| positive_id(Some(&b.id)));
            let item = ChaptarrItem {
                book,
                display_title,
                cover,
                existing_book_id,
            };
            if let Some(&index) = seen.get(&key) {
                duplicate_count += 1;
                if format_affinity > item_affinities[index] {
                    items[index] = item;
                    item_affinities[index] = format_affinity;
                }
                continue;
            }
            seen.insert(key, items.len());
            items.push(item);
            item_affinities.push(format_affinity);
        }

        if self.openlibrary_covers && items.iter().any(|item| item.cover.is_none()) {
            let covers = self.open_library_covers(term).await;
            for item in &mut items {
                if item.cover.is_none() {
                    item.cover = covers
                        .get(&(
                            normalize(&item.book.title),
                            normalize(&item.book.author.author_name),
                        ))
                        .cloned();
                }
            }
        }
        let query = normalize(term);
        items.sort_by_key(|item| Reverse(search_rank(&item.book.title, &query)));
        info!(
            format = ?self.format,
            raw_count,
            accepted_count = items.len(),
            malformed_count,
            incomplete_identity_count,
            junk_count,
            duplicate_count,
            other_format_projection_count,
            "Chaptarr search complete"
        );
        Ok(items
            .into_iter()
            .map(|item| Box::new(item) as Box<dyn MediaItem>)
            .collect())
    }

    fn early_stop(&self, _media: &dyn MediaItem) -> bool {
        // Lookup-local IDs do not prove requested-format availability. The
        // authoritative, format-aware check is asynchronous and happens below.
        false
    }

    fn display_info(&self, media: &dyn MediaItem) -> MediaDisplayInfo {
        let Some(item) = media.as_any().downcast_ref::<ChaptarrItem>() else {
            return MediaDisplayInfo {
                title: String::new(),
                subtitle: None,
                description: None,
                thumbnail_url: None,
            };
        };
        MediaDisplayInfo {
            title: item.book.title.clone(),
            subtitle: (!item.book.author.author_name.is_empty())
                .then(|| format!("by {}", item.book.author.author_name)),
            description: Some(strip_html(&item.book.overview)).filter(|s| !s.is_empty()),
            thumbnail_url: item.cover.clone(),
        }
    }

    async fn additional_details(&self, media: &dyn MediaItem) -> Result<Vec<RequestDetails>> {
        let item = media
            .as_any()
            .downcast_ref::<ChaptarrItem>()
            .context("Invalid media type for Chaptarr")?;
        validate_item(item)?;
        if let Some(state) = self.already_requested(item).await? {
            let message = match state {
                FormatState::Available => format!(
                    "{} is already available as an {}.",
                    item.book.title,
                    format_name(self.format)
                ),
                FormatState::Processing => format!(
                    "{} is already requested as an {}.",
                    item.book.title,
                    format_name(self.format)
                ),
                FormatState::Missing => unreachable!(),
            };
            bail!(UserFacingError(message));
        }
        Ok(Vec::new())
    }

    async fn request(
        &self,
        _details: Vec<RequestDetails>,
        media: Box<dyn MediaItem>,
        _requester_discord_id: u64,
    ) -> Result<()> {
        let item = *media
            .into_any()
            .downcast::<ChaptarrItem>()
            .map_err(|_| anyhow::anyhow!("Invalid media type for Chaptarr"))?;
        validate_item(&item)?;

        let author_key = format!("{}:{}", self.base_url, item.book.author.foreign_author_id);
        let work_lock = {
            let mut in_flight = mutation_locks().lock().await;
            in_flight.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = in_flight.get(&author_key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                in_flight.insert(author_key, Arc::downgrade(&lock));
                lock
            }
        };
        let _request_guard = work_lock.lock().await;

        // Re-read inside the per-work lock. Two simultaneous Discord clicks for
        // the same work+format cannot both pass this idempotency boundary. The
        // state check spans every matching local row, so an available or
        // in-flight duplicate pocket also stops a second request.
        let (mut author, mut book, rows) = self.locate_existing(&item).await?;
        match self.request_state_across(&rows, &item).await? {
            FormatState::Available => bail!(UserFacingError(format!(
                "{} is already available as an {}.",
                item.book.title,
                format_name(self.format)
            ))),
            FormatState::Processing => bail!(UserFacingError(format!(
                "{} is already requested as an {}.",
                item.book.title,
                format_name(self.format)
            ))),
            FormatState::Missing => {}
        }

        let mut author_added = false;
        if author.is_none() {
            let response = self
                .send_json(Method::POST, "/book", &self.new_author_body(&item))
                .await?;
            if let Some(author_id) = positive_id(response.get("authorId"))
                .or_else(|| positive_id(response.pointer("/author/id")))
            {
                author = Some(self.get(&format!("/author/{author_id}"), &[]).await?);
            } else {
                // Some Chaptarr builds acknowledge the add without echoing the
                // local author ID. Re-resolve the stable external identity
                // instead of treating a response shape as authoritative.
                author = self.find_author(&item).await?;
            }
            author_added = true;
        }

        let mut author_id = author
            .as_ref()
            .and_then(|a| positive_id(a.get("id")))
            .context("Chaptarr could not resolve the requested author")?;
        if author_added {
            // A brand-new author's catalog import continues long after the
            // target row first appears. Monitoring before it settles is how a
            // request dies silently, so nothing below runs until it has.
            self.wait_for_catalog_settle(
                author_id,
                &item.book.author.author_name,
                &item.book,
                true,
            )
            .await?;
        } else {
            // A retry sees an existing author even when the original catalog
            // import is still running. Even an idle command sample is not
            // enough: live imports can mutate editions after that command
            // disappears. Every existing-author write therefore requires the
            // full three-snapshot catalog/edition stability gate.
            self.wait_for_catalog_settle(
                author_id,
                &item.book.author.author_name,
                &item.book,
                book.is_some(),
            )
            .await?;
            let (refreshed_author, refreshed_book, refreshed_rows) =
                self.locate_existing(&item).await?;
            if let Some(refreshed_author) = refreshed_author {
                author_id = positive_id(refreshed_author.get("id"))
                    .context("Chaptarr could not re-resolve the requested author")?;
                author = Some(refreshed_author);
            }
            book = refreshed_book;
            match self.request_state_across(&refreshed_rows, &item).await? {
                FormatState::Available => bail!(UserFacingError(format!(
                    "{} is already available as an {}.",
                    item.book.title,
                    format_name(self.format)
                ))),
                FormatState::Processing => bail!(UserFacingError(format!(
                    "{} is already requested as an {}.",
                    item.book.title,
                    format_name(self.format)
                ))),
                FormatState::Missing => {}
            }
            if book.is_none() {
                let local_author = author
                    .as_ref()
                    .context("Chaptarr could not resolve the requested author")?;
                let body = self.existing_author_book_body(&item, local_author)?;
                self.send_json(Method::POST, "/book", &body).await?;
                // Adding one work can itself start metadata work. Do not let
                // the first visible row escape into monitoring; require the
                // newly added target and its edition list to settle first.
                self.wait_for_catalog_settle(
                    author_id,
                    &item.book.author.author_name,
                    &item.book,
                    true,
                )
                .await?;
            }
        }
        if book.as_ref().is_none_or(|row| !book_complete(row)) {
            book = self.poll_target(author_id, &item.book).await?;
        }
        if needs_author_refresh(book.as_ref()) {
            // RefreshAuthor is intentionally guarded and only runs after the
            // user has pressed Request, when the exact target exists as an
            // unresolved placeholder. A missing target is never refreshed.
            // The refresh re-runs catalog work, so it gets the same settle
            // wait before any monitoring happens.
            self.send_json(
                Method::POST,
                "/command",
                &json!({"name": "RefreshAuthor", "authorId": author_id}),
            )
            .await?;
            self.wait_for_catalog_settle(
                author_id,
                &item.book.author.author_name,
                &item.book,
                true,
            )
            .await?;
            book = self.poll_target(author_id, &item.book).await?;
        }
        let book = book.ok_or_else(|| {
            UserFacingError(format!(
                "Chaptarr could not resolve this {} to a safe local book row. Try refreshing the author in Chaptarr.",
                format_name(self.format)
            ))
        })?;
        if !local_row_matches_item(&book, self.format, &item.book) {
            bail!(UserFacingError(
                "Chaptarr resolved a different work, so nothing was monitored or searched.".into()
            ));
        }
        if !book_complete(&book) {
            bail!(UserFacingError(format!(
                "Chaptarr only has an unresolved placeholder for this {}. Try refreshing the author in Chaptarr.",
                format_name(self.format)
            )));
        }

        // Re-resolve edition-aware: duplicate import pockets are deduped by
        // which row carries usable requested-format editions, and the edition
        // to monitor is chosen from `/edition` data (the book resource always
        // reports an empty editions array and must never be trusted for this).
        let (book, editions) = self
            .resolve_pocket(author_id, &item.book)
            .await?
            .ok_or_else(|| {
                UserFacingError(format!(
                    "Chaptarr could not resolve this {} to a safe local book row. Try refreshing the author in Chaptarr.",
                    format_name(self.format)
                ))
            })?;
        if !book_complete(&book) {
            bail!(UserFacingError(format!(
                "Chaptarr only has an unresolved placeholder for this {}. Try refreshing the author in Chaptarr.",
                format_name(self.format)
            )));
        }
        let Some(chosen) = preferred_edition_index(&editions, self.format, &item.book) else {
            bail!(UserFacingError(format!(
                "Chaptarr has no usable {} edition of {}, so nothing was monitored or searched. The book needs attention in Chaptarr.",
                format_name(self.format),
                item.book.title
            )));
        };
        let book_id = positive_id(book.get("id")).context("Resolved Chaptarr book has no id")?;

        self.enable_author_format(author_id).await?;
        self.select_edition(&book, &editions, chosen).await?;
        // Book-level monitoring only persists through this dedicated endpoint;
        // the full-book PUT above silently ignores monitored flags.
        self.send_json(
            Method::PUT,
            "/book/monitor",
            &json!({"bookIds": [book_id], "monitored": true}),
        )
        .await?;
        self.verify_request_ready(book_id, &item, &editions[chosen])
            .await?;
        let acknowledgement = match self
            .send_json(
                Method::POST,
                "/command",
                &json!({"name": "BookSearch", "bookIds": [book_id]}),
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                warn!(book_id, %error, "Chaptarr did not confirm BookSearch");
                bail!(UserFacingError(format!(
                    "Chaptarr kept the {} request settings but did not confirm that its search started. Request {} again to safely retry the search.",
                    format_name(self.format),
                    item.book.title
                )));
            }
        };
        if !valid_book_search_acknowledgement(&acknowledgement) {
            warn!(
                book_id,
                ?acknowledgement,
                "Chaptarr returned an invalid BookSearch acknowledgement"
            );
            bail!(UserFacingError(format!(
                "Chaptarr kept the {} request settings but did not confirm that its search started. Request {} again to safely retry the search.",
                format_name(self.format),
                item.book.title
            )));
        }
        self.record_search_acknowledgement(book_id).await;
        Ok(())
    }

    fn success_message(
        &self,
        _details: &[RequestDetails],
        media: &dyn MediaItem,
    ) -> SuccessMessage {
        let Some(item) = media.as_any().downcast_ref::<ChaptarrItem>() else {
            return SuccessMessage {
                summary: "Book requested".into(),
                description: "Chaptarr is searching for it.".into(),
                thumbnail_url: None,
                embed_data: None,
            };
        };
        let summary = format!("{} — {}", item.book.title, item.book.author.author_name);
        let embed_data = item.cover.as_ref().map(|cover| EmbedData {
            title: summary.clone(),
            media_type: match self.format {
                ChaptarrFormat::Ebook => "Ebook",
                ChaptarrFormat::Audiobook => "Audiobook",
            },
            overview: truncate_for_embed(&strip_html(&item.book.overview)),
            poster_url: cover.clone(),
            genres: Vec::new(),
            runtime_minutes: None,
            studio_or_network: None,
            director: None,
            external_url: open_library_work_search(&item.book),
        });
        SuccessMessage {
            summary,
            description: format!(
                "Requested as an {}. Chaptarr will download it when available.",
                format_name(self.format)
            ),
            thumbnail_url: item.cover.clone(),
            embed_data,
        }
    }
}

fn validate_item(item: &ChaptarrItem) -> Result<()> {
    if clear_multi_book_result_title(&item.book.title) {
        bail!(UserFacingError(format!(
            "{} looks like a multi-book collection. Request each individual title so Chaptarr can select and verify the right edition.",
            item.book.title
        )));
    }
    if book_identity_complete(&item.book) {
        Ok(())
    } else {
        bail!(UserFacingError(
            "That Chaptarr result is missing a stable book or author identity, so it was not requested."
                .into()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const LOOKUP: &str = include_str!("../../tests/fixtures/chaptarr/lookup.json");
    const AUDIOBOOK_PROJECTION_LOOKUP: &str =
        include_str!("../../tests/fixtures/chaptarr/lookup_audiobook_projection.json");
    const AUTHOR: &str = include_str!("../../tests/fixtures/chaptarr/author.json");
    const COMMAND_RESPONSE: &str =
        include_str!("../../tests/fixtures/chaptarr/command_response.json");

    fn backend(format: ChaptarrFormat) -> Chaptarr {
        Chaptarr {
            client: reqwest::Client::new(),
            base_url: "http://chaptarr.test/api/v1".into(),
            api_key: "test-only".into(),
            server_version: "0.9.720.0".into(),
            format,
            openlibrary_covers: false,
            settle: SettlePacing {
                interval: Duration::from_millis(20),
                deadline: Duration::from_secs(5),
            },
            settings: ResolvedSettings {
                ebook_root: "/library/ebooks".into(),
                audiobook_root: "/library/audiobooks".into(),
                ebook_quality: 11,
                audiobook_quality: 12,
                ebook_metadata: 21,
                audiobook_metadata: 22,
            },
        }
    }

    fn lookup_item() -> ChaptarrItem {
        let rows: Vec<Value> = serde_json::from_str(LOOKUP).unwrap();
        let book: BookShape = serde_json::from_value(rows[0].clone()).unwrap();
        ChaptarrItem {
            display_title: format!("{} — {}", book.title, book.author.author_name),
            cover: absolute_cover(&book, ChaptarrFormat::Ebook),
            existing_book_id: book
                .local_ebook_books
                .iter()
                .find_map(|row| positive_id(Some(&row.id))),
            book,
        }
    }

    fn audiobook_projection_item() -> ChaptarrItem {
        let rows: Vec<Value> = serde_json::from_str(AUDIOBOOK_PROJECTION_LOOKUP).unwrap();
        let book: BookShape = serde_json::from_value(rows[0].clone()).unwrap();
        ChaptarrItem {
            display_title: format!("{} — {}", book.title, book.author.author_name),
            cover: None,
            existing_book_id: None,
            book,
        }
    }

    async fn mock_api(
        responses: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            for body in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = stream.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                recorded
                    .lock()
                    .await
                    .push(String::from_utf8_lossy(&request).into_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        (format!("http://{address}/api/v1"), requests, server)
    }

    fn request_line(request: &str) -> &str {
        request.lines().next().unwrap_or_default()
    }

    #[test]
    fn missing_identity_is_rejected() {
        let mut item = lookup_item();
        item.book.foreign_book_id.clear();
        assert!(validate_item(&item).is_err());
        item.book.foreign_book_id = "hc:work-1001".into();
        item.book.author.foreign_author_id.clear();
        assert!(validate_item(&item).is_err());
    }

    #[test]
    fn book_search_acknowledgement_requires_a_known_success_state() {
        for status in ["queued", "started", "completed"] {
            assert!(valid_book_search_acknowledgement(&json!({
                "id": 7101,
                "commandName": "BookSearch",
                "status": status
            })));
        }
        for response in [
            json!({"id": 7101, "name": "BookSearch", "status": "failed"}),
            json!({"id": 7101, "name": "BookSearch", "status": "unexpected"}),
            json!({"id": 0, "name": "BookSearch", "status": "queued"}),
            json!({"id": 7101, "name": "RefreshAuthor", "status": "queued"}),
        ] {
            assert!(!valid_book_search_acknowledgement(&response));
        }
    }

    #[test]
    fn existing_author_body_is_allowlisted_and_all_unmonitored() {
        let item = lookup_item();
        let author: Value = serde_json::from_str(AUTHOR).unwrap();
        let body = backend(ChaptarrFormat::Ebook)
            .existing_author_book_body(&item, &author)
            .unwrap();
        assert_eq!(body["id"], 0);
        assert_eq!(body["mediaType"], "ebook");
        assert_eq!(body["monitored"], false);
        assert_eq!(body["ebookMonitored"], false);
        assert_eq!(body["audiobookMonitored"], false);
        assert!(body.get("remoteCover").is_none());
        assert!(body.get("localEbookBooks").is_none());
        assert!(
            body["editions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|edition| edition["monitored"] == false)
        );
    }

    #[test]
    fn existing_author_audiobook_body_replaces_unsafe_projections_with_neutral_placeholder() {
        let mut item = lookup_item();
        item.book.editions = serde_json::from_value(json!([
            {
                "title": "The Clockwork Orchard hardcover",
                "foreignEditionId": "hc:physical-1001",
                "format": "physical",
                "isEbook": false
            },
            {
                "title": "Untyped projection that might be physical",
                "foreignEditionId": "hc:untyped-1001",
                "isEbook": false
            }
        ]))
        .unwrap();
        let author: Value = serde_json::from_str(AUTHOR).unwrap();
        let body = backend(ChaptarrFormat::Audiobook)
            .existing_author_book_body(&item, &author)
            .unwrap();
        let editions = body["editions"].as_array().unwrap();
        assert_eq!(editions.len(), 1);
        assert!(editions[0].get("foreignEditionId").is_none());
        assert_eq!(editions[0]["format"], "audiobook");
        assert_eq!(editions[0]["isEbook"], false);
        assert_eq!(editions[0]["monitored"], false);
    }

    #[test]
    fn new_author_body_contains_both_format_settings_but_no_book_monitoring() {
        let item = lookup_item();
        let body = backend(ChaptarrFormat::Audiobook).new_author_body(&item);
        assert_eq!(body["mediaType"], "audiobook");
        assert_eq!(body["rootFolderPath"], "/library/audiobooks");
        assert_eq!(body["ebookQualityProfileId"], 11);
        assert_eq!(body["audiobookQualityProfileId"], 12);
        assert_eq!(body["ebookMetadataProfileId"], 21);
        assert_eq!(body["audiobookMetadataProfileId"], 22);
        assert_eq!(body["monitored"], false);
        assert_eq!(body["ebookMonitored"], false);
        assert_eq!(body["audiobookMonitored"], false);
        assert_eq!(body["author"]["audiobookMonitorFuture"], true);
    }

    #[test]
    fn new_author_ebook_body_overrides_audiobook_lookup_projection() {
        let item = audiobook_projection_item();
        assert_eq!(item.book.media_type, "audiobook");

        let body = backend(ChaptarrFormat::Ebook).new_author_body(&item);

        assert_eq!(body["mediaType"], "ebook");
        assert_eq!(body["rootFolderPath"], "/library/ebooks");
        assert_eq!(body["author"]["ebookMonitorFuture"], true);
        assert_eq!(body["author"]["audiobookMonitorFuture"], false);
        assert_eq!(body["monitored"], false);
    }

    #[tokio::test]
    async fn multi_book_collection_is_rejected_before_any_http_request() {
        let mut item = lookup_item();
        item.book.title = "The Clockwork Orchard Trilogy 3-Book Bundle".into();
        let error = backend(ChaptarrFormat::Ebook)
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<UserFacingError>().is_some());
        assert!(error.to_string().contains("individual title"));
    }

    #[tokio::test]
    async fn search_and_confirmation_use_only_get_requests() {
        let (base_url, requests, server) =
            mock_api(vec![AUDIOBOOK_PROJECTION_LOOKUP.into(), "[]".to_string()]).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();

        let results = chaptarr.search("Clockwork Orchard").await.unwrap();
        assert_eq!(results.len(), 1);
        let item = results[0].as_any().downcast_ref::<ChaptarrItem>().unwrap();
        assert_eq!(item.book.media_type, "audiobook");
        assert_eq!(item.existing_book_id, None);
        assert!(
            chaptarr
                .additional_details(results[0].as_ref())
                .await
                .unwrap()
                .is_empty()
        );
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        let recorded = requests.lock().await;
        assert_eq!(recorded.len(), 2);
        assert!(request_line(&recorded[0]).starts_with("GET /api/v1/book/lookup?"));
        assert_eq!(request_line(&recorded[1]), "GET /api/v1/author HTTP/1.1");
        assert!(
            recorded
                .iter()
                .all(|request| request_line(request).starts_with("GET "))
        );
    }

    #[tokio::test]
    async fn cross_format_projection_uses_only_the_requested_local_id_bridge() {
        let lookup = json!([{
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "audiobook",
            "author": {
                "authorName": "Mara Vale",
                "foreignAuthorId": "hc:author-1001"
            },
            "editions": [{"isEbook": false}],
            "localEbookBooks": [{"id": 4101}],
            "localAudiobookBooks": [{"id": 5101}]
        }]);
        let (base_url, _requests, server) = mock_api(vec![lookup.to_string()]).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;

        let results = chaptarr.search("Clockwork Orchard").await.unwrap();
        assert_eq!(results.len(), 1);
        let item = results[0].as_any().downcast_ref::<ChaptarrItem>().unwrap();
        assert_eq!(item.book.media_type, "audiobook");
        assert_eq!(item.existing_book_id, Some(4101));
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();
    }

    #[tokio::test]
    async fn requested_format_projection_wins_when_lookup_repeats_a_work() {
        let lookup = json!([
            {
                "title": "The Clockwork Orchard",
                "foreignBookId": "hc:work-1001",
                "mediaType": "audiobook",
                "author": {
                    "authorName": "Mara Vale",
                    "foreignAuthorId": "hc:author-1001"
                },
                "editions": [{"isEbook": false}],
                "localEbookBooks": []
            },
            {
                "title": "The Clockwork Orchard",
                "foreignBookId": "hc:work-1001",
                "mediaType": "ebook",
                "author": {
                    "authorName": "Mara Vale",
                    "foreignAuthorId": "hc:author-1001"
                },
                "editions": [{"isEbook": true}],
                "localEbookBooks": [{"id": 4101}]
            }
        ]);
        let (base_url, _requests, server) = mock_api(vec![lookup.to_string()]).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;

        let results = chaptarr.search("Clockwork Orchard").await.unwrap();
        assert_eq!(results.len(), 1);
        let item = results[0].as_any().downcast_ref::<ChaptarrItem>().unwrap();
        assert_eq!(item.book.media_type, "ebook");
        assert_eq!(item.existing_book_id, Some(4101));
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();
    }

    #[tokio::test]
    async fn new_author_audiobook_request_settles_selects_edition_and_readbacks() {
        let author = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": false,
            "audiobookMonitorFuture": true
        });
        let audiobook = json!({
            "id": 5101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "audiobook",
            "monitored": false,
            "audiobookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:audio-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/audio.jpg"}]
        });
        let monitored_audiobook = json!({
            "id": 5101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "audiobook",
            "monitored": true,
            "audiobookMonitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:audio-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/audio.jpg"}]
        });
        let audio_edition = json!({
            "id": 9101,
            "bookId": 5101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:audio-1001",
            "format": "audiobook",
            "isEbook": false,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let monitored_audio_edition = json!({
            "id": 9101,
            "bookId": 5101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:audio-1001",
            "format": "audiobook",
            "isEbook": false,
            "language": "eng",
            "monitored": true,
            "manualAdd": true
        });
        let responses = vec![
            "[]".to_string(),
            json!({"authorId": 7001}).to_string(),
            author.to_string(),
            // Catalog settle: three stable samples of commands, book rows,
            // and the target's authoritative edition list.
            "[]".to_string(),
            json!([audiobook.clone()]).to_string(),
            json!([audio_edition.clone()]).to_string(),
            "[]".to_string(),
            json!([audiobook.clone()]).to_string(),
            json!([audio_edition.clone()]).to_string(),
            "[]".to_string(),
            json!([audiobook.clone()]).to_string(),
            json!([audio_edition.clone()]).to_string(),
            // Target resolution, then edition-aware pocket resolution.
            json!([audiobook.clone()]).to_string(),
            json!([audiobook]).to_string(),
            json!([audio_edition]).to_string(),
            // Author format flag is already enabled by the add payload.
            author.to_string(),
            // Edition-select PUT, monitor PUT, then the full read-back.
            "{}".to_string(),
            "{}".to_string(),
            monitored_audiobook.to_string(),
            json!([monitored_audio_edition]).to_string(),
            COMMAND_RESPONSE.to_string(),
        ];
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Audiobook);
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let mut item = lookup_item();
        item.existing_book_id = None;

        chaptarr
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap();
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(
            lines,
            vec![
                "GET /api/v1/author HTTP/1.1",
                "POST /api/v1/book HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "GET /api/v1/command HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/edition?bookId=5101 HTTP/1.1",
                "GET /api/v1/command HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/edition?bookId=5101 HTTP/1.1",
                "GET /api/v1/command HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/edition?bookId=5101 HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/edition?bookId=5101 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/book/5101 HTTP/1.1",
                "PUT /api/v1/book/monitor HTTP/1.1",
                "GET /api/v1/book/5101 HTTP/1.1",
                "GET /api/v1/edition?bookId=5101 HTTP/1.1",
                "POST /api/v1/command HTTP/1.1",
            ]
        );
        assert!(recorded[1].contains("\"rootFolderPath\":\"/library/audiobooks\""));
        assert!(recorded[1].contains("\"monitored\":false"));
        assert!(recorded[1].contains("\"ebookMonitored\":false"));
        assert!(recorded[1].contains("\"audiobookMonitored\":false"));
        assert!(recorded[1].contains("\"ebookMonitorFuture\":false"));
        assert!(recorded[1].contains("\"audiobookMonitorFuture\":true"));
        // The edition-select PUT mirrors the Chaptarr UI: complete book body,
        // anyEditionOk off, and exactly one monitored, manually-added edition.
        assert!(recorded[16].contains("\"anyEditionOk\":false"));
        assert!(recorded[16].contains("\"manualAdd\":true"));
        assert_eq!(recorded[16].matches("\"monitored\":true").count(), 1);
        assert!(recorded[17].contains("\"bookIds\":[5101]"));
        assert!(recorded[17].contains("\"monitored\":true"));
        assert!(recorded[20].contains("\"name\":\"BookSearch\""));
        assert!(recorded[20].contains("\"bookIds\":[5101]"));
    }

    #[tokio::test]
    async fn new_author_request_fails_loudly_while_catalog_is_importing() {
        let author = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": false,
            "audiobookMonitorFuture": true
        });
        let refresh_in_flight = json!([{
            "name": "RefreshAuthor",
            "status": "started",
            "body": {"authorId": 7001}
        }]);
        let responses = vec![
            "[]".to_string(),
            json!({"authorId": 7001}).to_string(),
            author.to_string(),
            refresh_in_flight.to_string(),
        ];
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Audiobook);
        chaptarr.settle.deadline = Duration::ZERO;
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let mut item = lookup_item();
        item.existing_book_id = None;

        let error = chaptarr
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<UserFacingError>().is_some());
        assert!(error.to_string().contains("NOT completed"));
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        // The author add is the only mutation: nothing was monitored, no
        // edition was selected, and no search was queued.
        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(
            lines,
            vec![
                "GET /api/v1/author HTTP/1.1",
                "POST /api/v1/book HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "GET /api/v1/command HTTP/1.1",
            ]
        );
    }

    async fn assert_strict_readback_failure(generic_monitored: bool, format_monitored: bool) {
        let author = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": true,
            "audiobookMonitorFuture": false
        });
        let book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let failed_readback = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": generic_monitored,
            "ebookMonitored": format_monitored,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let mut responses = vec![
            json!([author.clone()]).to_string(),
            json!([book.clone()]).to_string(),
            // Exact-search preflight runs even when import reverted both
            // monitor flags.
            "[]".to_string(),
        ];
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([book.clone()]).to_string(),
                json!([edition.clone()]).to_string(),
            ]);
        }
        responses.extend([
            json!([author.clone()]).to_string(),
            json!([book.clone()]).to_string(),
            "[]".to_string(),
            json!([book]).to_string(),
            json!([edition]).to_string(),
            author.to_string(),
            "{}".to_string(),
            "{}".to_string(),
            failed_readback.to_string(),
        ]);
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        let mut item = lookup_item();
        item.existing_book_id = None;

        let error = chaptarr
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<UserFacingError>().is_some());
        assert!(error.to_string().contains("no search was queued"));
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();
        let recorded = requests.lock().await;
        assert!(
            recorded
                .iter()
                .all(|request| !request.contains("\"name\":\"BookSearch\""))
        );
    }

    #[tokio::test]
    async fn strict_readback_requires_both_generic_and_format_monitoring() {
        assert_strict_readback_failure(true, false).await;
        assert_strict_readback_failure(false, true).await;
    }

    #[tokio::test]
    async fn partial_or_import_reverted_state_still_dedupes_an_active_search() {
        let partial = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true,
            "hasFiles": false
        });
        let import_reverted = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false
        });
        let active = json!([{
            "id": 7101,
            "commandName": "BookSearch",
            "status": "started",
            "body": {"bookIds": [4101]}
        }]);
        let (base_url, _requests, server) =
            mock_api(vec!["[]".to_string(), active.to_string()]).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        let item = lookup_item();

        assert_eq!(
            chaptarr
                .request_state_across(std::slice::from_ref(&partial), &item)
                .await
                .unwrap(),
            FormatState::Missing
        );
        assert_eq!(
            chaptarr
                .request_state_across(std::slice::from_ref(&import_reverted), &item)
                .await
                .unwrap(),
            FormatState::Processing
        );
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();
    }

    #[tokio::test]
    async fn catalog_settle_resets_for_active_changed_and_unknown_command_state() {
        let active_refresh = json!([{
            "id": 7201,
            "name": "RefreshAuthor",
            "status": "started",
            "body": {"authorId": 7001}
        }]);
        let book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let edition_one = json!([{
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng"
        }]);
        let edition_two = json!([
            {
                "id": 8101,
                "bookId": 4101,
                "title": "The Clockwork Orchard",
                "foreignEditionId": "hc:edition-1001",
                "format": "ebook",
                "isEbook": true,
                "language": "eng"
            },
            {
                "id": 8102,
                "bookId": 4101,
                "title": "The Clockwork Orchard",
                "foreignEditionId": "hc:edition-1002",
                "format": "ebook",
                "isEbook": true,
                "language": "eng"
            }
        ]);
        let mut responses = vec![active_refresh.to_string()];
        for editions in [&edition_one, &edition_two, &edition_two] {
            responses.extend([
                "[]".to_string(),
                json!([book.clone()]).to_string(),
                editions.to_string(),
            ]);
        }
        // Invalid JSON makes the command poll unknown and must erase the two
        // quiet samples already accumulated for edition_two.
        responses.push("not-json".to_string());
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([book.clone()]).to_string(),
                edition_two.to_string(),
            ]);
        }
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        let item = lookup_item();

        chaptarr
            .wait_for_catalog_settle(7001, "Mara Vale", &item.book, true)
            .await
            .unwrap();
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();
        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "GET /api/v1/command HTTP/1.1")
                .count(),
            8
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "GET /api/v1/edition?bookId=4101 HTTP/1.1")
                .count(),
            6
        );
        assert!(lines.iter().all(|line| line.starts_with("GET ")));
    }

    #[tokio::test]
    async fn existing_author_missing_work_posts_allowlisted_body_then_verifies() {
        let author_unmonitored = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": false,
            "audiobookMonitorFuture": false
        });
        let author_monitored = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": true,
            "audiobookMonitorFuture": false
        });
        let resolved_book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let monitored_book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let ebook_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let monitored_ebook_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": true,
            "manualAdd": true
        });
        let mut responses = vec![
            json!([author_unmonitored.clone()]).to_string(),
            "[]".to_string(),
        ];
        // Even an empty existing catalog requires three quiet snapshots.
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend(["[]".to_string(), "[]".to_string()]);
        }
        responses.extend([
            json!([author_unmonitored.clone()]).to_string(),
            "[]".to_string(),
            json!({
                "id": 9999,
                "authorId": 7001,
                "title": "The Clockwork Orchard",
                "foreignBookId": "hc:work-other",
                "mediaType": "ebook",
                "releaseDate": "2024-01-01",
                "foreignEditionId": "hc:edition-other",
                "images": [{"url": "https://covers.example.test/wrong.jpg"}]
            })
            .to_string(),
        ]);
        // POST /book can start its own import tail. The target row and
        // authoritative editions must be unchanged for the full window.
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([resolved_book.clone()]).to_string(),
                json!([ebook_edition.clone()]).to_string(),
            ]);
        }
        responses.extend([
            json!([resolved_book.clone()]).to_string(),
            json!([resolved_book]).to_string(),
            json!([ebook_edition]).to_string(),
            author_unmonitored.to_string(),
            "{}".to_string(),
            author_monitored.to_string(),
            "{}".to_string(),
            "{}".to_string(),
            monitored_book.to_string(),
            json!([monitored_ebook_edition]).to_string(),
            COMMAND_RESPONSE.to_string(),
        ]);
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let mut item = lookup_item();
        item.existing_book_id = None;

        chaptarr
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap();
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(lines.len(), 31);
        assert_eq!(lines[10], "POST /api/v1/book HTTP/1.1");
        assert!(recorded[10].contains("\"foreignBookId\":\"hc:work-1001\""));
        assert!(recorded[10].contains("\"monitored\":false"));
        assert!(recorded[10].contains("\"ebookMonitored\":false"));
        assert!(recorded[10].contains("\"audiobookMonitored\":false"));
        assert!(!recorded[10].contains("remoteCover"));
        assert!(recorded[26].contains("\"anyEditionOk\":false"));
        assert!(recorded[26].contains("\"manualAdd\":true"));
        assert_eq!(recorded[26].matches("\"monitored\":true").count(), 1);
        assert_eq!(lines[27], "PUT /api/v1/book/monitor HTTP/1.1");
        assert_eq!(lines[30], "POST /api/v1/command HTTP/1.1");
        assert_eq!(
            lines[11..27]
                .iter()
                .filter(|line| **line == "GET /api/v1/edition?bookId=4101 HTTP/1.1")
                .count(),
            4
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "POST /api/v1/command HTTP/1.1")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn malformed_search_acknowledgement_leaves_a_retryable_request() {
        let author = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": true,
            "audiobookMonitorFuture": false
        });
        let unmonitored_book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let monitored_book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let unmonitored_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let monitored_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": true,
            "manualAdd": true
        });
        let mut responses = vec![
            json!([author.clone()]).to_string(),
            json!([unmonitored_book.clone()]).to_string(),
            "[]".to_string(),
        ];
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([unmonitored_book.clone()]).to_string(),
                json!([unmonitored_edition.clone()]).to_string(),
            ]);
        }
        responses.extend([
            json!([author.clone()]).to_string(),
            json!([unmonitored_book.clone()]).to_string(),
            "[]".to_string(),
            json!([unmonitored_book.clone()]).to_string(),
            json!([unmonitored_edition.clone()]).to_string(),
            author.to_string(),
            "{}".to_string(),
            "{}".to_string(),
            monitored_book.to_string(),
            json!([monitored_edition.clone()]).to_string(),
            // HTTP success without a command identity must not produce a
            // Discord success or enter the recent-ack dedupe cache.
            "{}".to_string(),
            json!([author.clone()]).to_string(),
            json!([monitored_book.clone()]).to_string(),
            // The retry proves no exact search is active, then runs the full
            // stability gate before repairing the persisted partial state.
            "[]".to_string(),
        ]);
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([monitored_book.clone()]).to_string(),
                json!([monitored_edition.clone()]).to_string(),
            ]);
        }
        responses.extend([
            json!([author.clone()]).to_string(),
            json!([monitored_book.clone()]).to_string(),
            "[]".to_string(),
            json!([monitored_book.clone()]).to_string(),
            json!([monitored_edition.clone()]).to_string(),
            author.to_string(),
            "{}".to_string(),
            "{}".to_string(),
            monitored_book.to_string(),
            json!([monitored_edition]).to_string(),
            json!({"id": 7102, "name": "BookSearch", "status": "queued"}).to_string(),
        ]);
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        let mut item = lookup_item();
        item.existing_book_id = None;
        let retry_item = item.clone();

        let first_error = chaptarr
            .request(Vec::new(), Box::new(item), 1234)
            .await
            .unwrap_err();
        assert!(first_error.downcast_ref::<UserFacingError>().is_some());
        assert!(
            first_error
                .to_string()
                .contains("did not confirm that its search started")
        );
        chaptarr
            .request(Vec::new(), Box::new(retry_item), 1234)
            .await
            .unwrap();
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(lines.len(), 46);
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "GET /api/v1/command HTTP/1.1")
                .count(),
            10
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "GET /api/v1/edition?bookId=4101 HTTP/1.1")
                .count(),
            10
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "PUT /api/v1/book/4101 HTTP/1.1")
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "PUT /api/v1/book/monitor HTTP/1.1")
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "POST /api/v1/command HTTP/1.1")
                .count(),
            2
        );
        assert!(
            lines
                .iter()
                .all(|line| *line != "PUT /api/v1/author/7001 HTTP/1.1")
        );
        assert!(recorded[45].contains("\"name\":\"BookSearch\""));
    }

    #[tokio::test]
    async fn concurrent_existing_work_waits_for_edition_stability_and_queues_one_search() {
        let author_unmonitored = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": false,
            "audiobookMonitorFuture": false
        });
        let author_monitored = json!({
            "id": 7001,
            "authorName": "Mara Vale",
            "foreignAuthorId": "hc:author-1001",
            "monitored": true,
            "ebookMonitorFuture": true,
            "audiobookMonitorFuture": false
        });
        let book_unmonitored = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let book_monitored = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-1001",
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let ebook_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let monitored_ebook_edition = json!({
            "id": 8101,
            "bookId": 4101,
            "title": "The Clockwork Orchard",
            "foreignEditionId": "hc:edition-1001",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": true,
            "manualAdd": true
        });
        let second_edition = json!({
            "id": 8102,
            "bookId": 4101,
            "title": "The Clockwork Orchard: Annotated",
            "foreignEditionId": "hc:edition-1002",
            "format": "ebook",
            "isEbook": true,
            "language": "eng",
            "monitored": false,
            "manualAdd": false
        });
        let expanded_editions = json!([ebook_edition.clone(), second_edition.clone()]);
        let monitored_editions = json!([monitored_ebook_edition, second_edition]);
        let mut responses = vec![
            json!([author_unmonitored.clone()]).to_string(),
            json!([book_unmonitored.clone()]).to_string(),
            // Exact-search preflight must run before the longer stability
            // window even though this row is currently unmonitored.
            "[]".to_string(),
            // The command is idle throughout, but the edition list grows
            // after the first sample. Stability must restart at that point.
            "[]".to_string(),
            json!([book_unmonitored.clone()]).to_string(),
            json!([ebook_edition]).to_string(),
        ];
        for _ in 0..SETTLE_STABLE_SAMPLES {
            responses.extend([
                "[]".to_string(),
                json!([book_unmonitored.clone()]).to_string(),
                expanded_editions.to_string(),
            ]);
        }
        responses.extend([
            json!([author_unmonitored.clone()]).to_string(),
            json!([book_unmonitored.clone()]).to_string(),
            "[]".to_string(),
            json!([book_unmonitored.clone()]).to_string(),
            expanded_editions.to_string(),
            author_unmonitored.to_string(),
            "{}".to_string(),
            author_monitored.to_string(),
            "{}".to_string(),
            "{}".to_string(),
            book_monitored.to_string(),
            monitored_editions.to_string(),
            COMMAND_RESPONSE.to_string(),
            json!([author_monitored.clone()]).to_string(),
            json!([book_monitored.clone()]).to_string(),
        ]);
        let (base_url, requests, server) = mock_api(responses).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let mut item = lookup_item();
        item.existing_book_id = None;

        let chaptarr = Arc::new(chaptarr);
        let second_item = item.clone();
        let (first, second) = tokio::join!(
            chaptarr.request(Vec::new(), Box::new(item), 1234),
            chaptarr.request(Vec::new(), Box::new(second_item), 5678),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock server should finish")
            .unwrap();

        let recorded = requests.lock().await;
        let lines: Vec<_> = recorded
            .iter()
            .map(|request| request_line(request))
            .collect();
        assert_eq!(lines.len(), 30);
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "GET /api/v1/edition?bookId=4101 HTTP/1.1")
                .count(),
            6
        );
        assert!(recorded[24].contains("\"bookIds\":[4101]"));
        assert!(recorded[24].contains("\"monitored\":true"));
        assert!(recorded[27].contains("\"name\":\"BookSearch\""));
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "POST /api/v1/command HTTP/1.1")
                .count(),
            1
        );
    }
}
