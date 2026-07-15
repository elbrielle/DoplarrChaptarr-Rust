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
const OPEN_LIBRARY_MIN_INTERVAL: Duration = Duration::from_secs(1);
const OPEN_LIBRARY_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const OPEN_LIBRARY_CACHE_CAPACITY: usize = 128;
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

fn open_library_state() -> &'static Mutex<OpenLibraryState> {
    OPEN_LIBRARY_STATE.get_or_init(|| Mutex::new(OpenLibraryState::default()))
}

fn mutation_locks() -> &'static Mutex<HashMap<String, Weak<Mutex<()>>>> {
    CHAPTARR_MUTATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
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

    async fn locate_existing(&self, item: &ChaptarrItem) -> Result<(Option<Value>, Option<Value>)> {
        if let Some(id) = item.existing_book_id {
            let book = self.get_book(id).await?;
            if local_row_matches_item(&book, self.format, &item.book) {
                let author_id = positive_id(book.get("authorId"));
                let author = if let Some(id) = author_id {
                    Some(self.get(&format!("/author/{id}"), &[]).await?)
                } else {
                    self.find_author(item).await?
                };
                return Ok((author, Some(book)));
            }
            warn!(
                book_id = id,
                format = ?self.format,
                "Ignoring lookup local-book id for the wrong format"
            );
        }
        let author = self.find_author(item).await?;
        let Some(author_id) = author.as_ref().and_then(|a| positive_id(a.get("id"))) else {
            return Ok((None, None));
        };
        let rows = self.books_for_author(author_id).await?;
        let book = preferred_book(&rows, self.format, &item.book);
        Ok((author, book))
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
            .filter(|edition| edition.is_ebook.is_none_or(|value| value == expected_ebook))
            .map(|edition| {
                json!({
                    "title": if edition.title.is_empty() { &item.book.title } else { &edition.title },
                    "foreignEditionId": null_if_empty(&edition.foreign_edition_id),
                    "isEbook": expected_ebook,
                    "isbn13": edition.isbn13,
                    "monitored": false,
                    "manualAdd": false
                })
            })
            .collect();
        let editions = if selected_editions.is_empty() {
            vec![json!({
                "title": item.book.title,
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
        let (_, book) = self.locate_existing(item).await?;
        let Some(book) = book else { return Ok(None) };
        let state = format_state(&book, self.format);
        Ok((state != FormatState::Missing).then_some(state))
    }
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
        // the same work+format cannot both pass this idempotency boundary.
        let (mut author, mut book) = self.locate_existing(&item).await?;
        if let Some(ref row) = book {
            match format_state(row, self.format) {
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
        }

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
        } else if let Some(local_author) = author.as_ref()
            && book.is_none()
        {
            let body = self.existing_author_book_body(&item, local_author)?;
            self.send_json(Method::POST, "/book", &body).await?;
        }

        let author_id = author
            .as_ref()
            .and_then(|a| positive_id(a.get("id")))
            .context("Chaptarr could not resolve the requested author")?;
        if book.as_ref().is_none_or(|row| !book_complete(row)) {
            book = self.poll_target(author_id, &item.book).await?;
        }
        if needs_author_refresh(book.as_ref()) {
            // RefreshAuthor is intentionally guarded and only runs after the
            // user has pressed Request, when the exact target exists as an
            // unresolved placeholder. A missing target is never refreshed.
            self.send_json(
                Method::POST,
                "/command",
                &json!({"name": "RefreshAuthor", "authorId": author_id}),
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
        let book_id = positive_id(book.get("id")).context("Resolved Chaptarr book has no id")?;

        self.enable_author_format(author_id).await?;
        self.send_json(
            Method::PUT,
            "/book/monitor",
            &json!({"bookIds": [book_id], "monitored": true}),
        )
        .await?;
        let verified = self.get_book(book_id).await?;
        if !local_row_matches_item(&verified, self.format, &item.book)
            || !format_is_monitored(&verified, self.format)
        {
            bail!(UserFacingError(
                "Chaptarr did not keep the requested format monitored, so no search was queued."
                    .into()
            ));
        }
        self.send_json(
            Method::POST,
            "/command",
            &json!({"name": "BookSearch", "bookIds": [book_id]}),
        )
        .await?;
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

    fn backend(format: ChaptarrFormat) -> Chaptarr {
        Chaptarr {
            client: reqwest::Client::new(),
            base_url: "http://chaptarr.test/api/v1".into(),
            api_key: "test-only".into(),
            server_version: "0.9.720.0".into(),
            format,
            openlibrary_covers: false,
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
    async fn new_author_audiobook_request_uses_safe_payload_and_readbacks() {
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
            "mediaType": "audiobook",
            "monitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:audio-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/audio.jpg"}]
        });
        let monitored_audiobook = json!({
            "id": 5101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "mediaType": "audiobook",
            "monitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:audio-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/audio.jpg"}]
        });
        let responses = vec![
            "[]".to_string(),
            json!({"authorId": 7001}).to_string(),
            author.to_string(),
            json!([audiobook]).to_string(),
            author.to_string(),
            "{}".to_string(),
            monitored_audiobook.to_string(),
            "{}".to_string(),
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
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/book/monitor HTTP/1.1",
                "GET /api/v1/book/5101 HTTP/1.1",
                "POST /api/v1/command HTTP/1.1",
            ]
        );
        assert!(recorded[1].contains("\"rootFolderPath\":\"/library/audiobooks\""));
        assert!(recorded[1].contains("\"monitored\":false"));
        assert!(recorded[1].contains("\"ebookMonitored\":false"));
        assert!(recorded[1].contains("\"audiobookMonitored\":false"));
        assert!(recorded[1].contains("\"ebookMonitorFuture\":false"));
        assert!(recorded[1].contains("\"audiobookMonitorFuture\":true"));
        assert!(recorded[5].contains("\"bookIds\":[5101]"));
        assert!(recorded[7].contains("\"name\":\"BookSearch\""));
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
            "mediaType": "ebook",
            "monitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let monitored_book = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "mediaType": "ebook",
            "monitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let responses = vec![
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
            json!([resolved_book]).to_string(),
            author_unmonitored.to_string(),
            "{}".to_string(),
            author_monitored.to_string(),
            "{}".to_string(),
            monitored_book.to_string(),
            "{}".to_string(),
        ];
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
        assert_eq!(
            lines,
            vec![
                "GET /api/v1/author HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "POST /api/v1/book HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/author/7001 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/book/monitor HTTP/1.1",
                "GET /api/v1/book/4101 HTTP/1.1",
                "POST /api/v1/command HTTP/1.1",
            ]
        );
        assert!(recorded[2].contains("\"foreignBookId\":\"hc:work-1001\""));
        assert!(recorded[2].contains("\"monitored\":false"));
        assert!(recorded[2].contains("\"ebookMonitored\":false"));
        assert!(recorded[2].contains("\"audiobookMonitored\":false"));
        assert!(!recorded[2].contains("remoteCover"));
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "POST /api/v1/command HTTP/1.1")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn concurrent_existing_work_requests_queue_one_search() {
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
            "mediaType": "ebook",
            "monitored": false,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let book_monitored = json!({
            "id": 4101,
            "authorId": 7001,
            "title": "The Clockwork Orchard",
            "mediaType": "ebook",
            "monitored": true,
            "hasFiles": false,
            "releaseDate": "2024-01-01",
            "foreignEditionId": "hc:edition-1001",
            "images": [{"coverType": "cover", "url": "https://covers.example.test/book.jpg"}]
        });
        let responses = vec![
            json!([author_unmonitored.clone()]).to_string(),
            json!([book_unmonitored.clone()]).to_string(),
            author_unmonitored.to_string(),
            "{}".to_string(),
            author_monitored.to_string(),
            "{}".to_string(),
            book_monitored.to_string(),
            "{}".to_string(),
            json!([author_monitored.clone()]).to_string(),
            json!([book_monitored.clone()]).to_string(),
        ];
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
        assert_eq!(
            lines,
            vec![
                "GET /api/v1/author HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/author/7001 HTTP/1.1",
                "GET /api/v1/author/7001 HTTP/1.1",
                "PUT /api/v1/book/monitor HTTP/1.1",
                "GET /api/v1/book/4101 HTTP/1.1",
                "POST /api/v1/command HTTP/1.1",
                "GET /api/v1/author HTTP/1.1",
                "GET /api/v1/book?authorId=7001 HTTP/1.1",
            ]
        );
        assert!(recorded[5].contains("\"bookIds\":[4101]"));
        assert!(recorded[5].contains("\"monitored\":true"));
        assert!(recorded[7].contains("\"name\":\"BookSearch\""));
        assert_eq!(
            lines
                .iter()
                .filter(|line| **line == "POST /api/v1/command HTTP/1.1")
                .count(),
            1
        );
    }
}
