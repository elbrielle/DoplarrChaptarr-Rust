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
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    any::Any,
    cmp::Reverse,
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, timeout},
};
use tracing::{debug, info, warn};

const API_PREFIX: &str = "/api/v1";
const OPEN_LIBRARY_SEARCH: &str = "https://openlibrary.org/search.json";
const RESOLVE_ATTEMPTS: usize = 20;
const RESOLVE_DEADLINE: Duration = Duration::from_secs(25);
const TESTED_CHAPTARR_VERSION: &str = "0.9.720";
const OPEN_LIBRARY_MIN_INTERVAL: Duration = Duration::from_secs(1);
const OPEN_LIBRARY_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const OPEN_LIBRARY_CACHE_CAPACITY: usize = 128;
const OPEN_LIBRARY_USER_AGENT: &str = concat!(
    "DoplarrChaptarr/",
    env!("CARGO_PKG_VERSION"),
    " (ebriellelucero@gmail.com)"
);

type CoverMap = HashMap<(String, String), String>;

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

fn open_library_cover_map(result: OpenLibraryResponse) -> CoverMap {
    let mut covers = HashMap::new();
    for doc in result.docs {
        let Some(id) = doc.cover_i.filter(|id| *id > 0) else {
            continue;
        };
        for author in doc.author_name {
            let key = (normalize(&doc.title), normalize(&author));
            if !key.0.is_empty() && !key.1.is_empty() {
                covers.entry(key).or_insert_with(|| {
                    format!("https://covers.openlibrary.org/b/id/{id}-L.jpg?default=false")
                });
            }
        }
    }
    covers
}

fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// Chaptarr has exposed the root-folder `ebook` and `audiobook` keys as both
/// booleans and nested settings objects. Only an explicit `true` is a format
/// discriminator; an object exists on both root types and must not select one.
fn bool_only<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Value>::deserialize(deserializer).map(|value| matches!(value, Some(Value::Bool(true))))
}

#[derive(Clone)]
pub struct Chaptarr {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    id: i32,
    name: String,
    #[serde(default)]
    profile_type: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootFolder {
    path: String,
    #[serde(default, deserialize_with = "null_default")]
    name: String,
    #[serde(default = "default_true", deserialize_with = "null_default")]
    accessible: bool,
    #[serde(default, deserialize_with = "bool_only")]
    ebook: bool,
    #[serde(default, deserialize_with = "bool_only")]
    audiobook: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_effective_default_ebook: bool,
    #[serde(default, deserialize_with = "null_default")]
    is_effective_default_audiobook: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchAuthor {
    #[serde(default, deserialize_with = "null_default")]
    author_name: String,
    #[serde(default, deserialize_with = "null_default")]
    foreign_author_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Image {
    #[serde(default, deserialize_with = "null_default")]
    url: String,
    #[serde(default, deserialize_with = "null_default")]
    cover_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Edition {
    #[serde(default)]
    is_ebook: Option<bool>,
    #[serde(default)]
    isbn13: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    title: String,
    #[serde(default, deserialize_with = "null_default")]
    foreign_edition_id: String,
    #[serde(default, deserialize_with = "null_default")]
    images: Vec<Image>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalBook {
    #[serde(default, deserialize_with = "null_default")]
    id: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Statistics {
    #[serde(default, deserialize_with = "null_default")]
    book_file_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    #[serde(default, deserialize_with = "null_default")]
    app_name: String,
    #[serde(default, deserialize_with = "null_default")]
    version: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ratings {
    #[serde(default, deserialize_with = "null_default")]
    popularity: f64,
    #[serde(default, deserialize_with = "null_default")]
    votes: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookShape {
    #[serde(default, deserialize_with = "null_default")]
    #[serde(alias = "bookTitle")]
    title: String,
    #[serde(default, deserialize_with = "null_default")]
    overview: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    foreign_book_id: String,
    #[serde(default, deserialize_with = "null_default")]
    foreign_edition_id: String,
    #[serde(default, deserialize_with = "null_default")]
    remote_cover: String,
    #[serde(default, deserialize_with = "null_default")]
    media_type: String,
    #[serde(default, deserialize_with = "null_default")]
    monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    ebook_monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    audiobook_monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    has_files: bool,
    #[serde(default, deserialize_with = "null_default")]
    grabbed: bool,
    #[serde(default, deserialize_with = "null_default")]
    author: SearchAuthor,
    #[serde(default, deserialize_with = "null_default")]
    images: Vec<Image>,
    #[serde(default, deserialize_with = "null_default")]
    editions: Vec<Edition>,
    #[serde(default, deserialize_with = "null_default")]
    local_ebook_books: Vec<LocalBook>,
    #[serde(default, deserialize_with = "null_default")]
    local_audiobook_books: Vec<LocalBook>,
    #[serde(default, deserialize_with = "null_default")]
    ebook_statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    audiobook_statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    ratings: Ratings,
}

#[derive(Debug, Clone)]
struct ChaptarrItem {
    book: BookShape,
    display_title: String,
    cover: Option<String>,
    existing_book_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatState {
    Available,
    Processing,
    Missing,
}

#[derive(Debug, Default, Deserialize)]
struct OpenLibraryResponse {
    #[serde(default, deserialize_with = "null_default")]
    docs: Vec<OpenLibraryDoc>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenLibraryDoc {
    #[serde(default, deserialize_with = "null_default")]
    title: String,
    #[serde(default, deserialize_with = "null_default")]
    author_name: Vec<String>,
    cover_i: Option<i64>,
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

        info!(format = ?format, url = %backend.base_url, "Connecting to Chaptarr");
        let (status, roots, quality, metadata) = tokio::try_join!(
            backend.get::<SystemStatus>("/system/status", &[]),
            backend.get::<Vec<RootFolder>>("/rootfolder", &[]),
            backend.get::<Vec<Profile>>("/qualityprofile", &[]),
            backend.get::<Vec<Profile>>("/metadataprofile", &[]),
        )?;
        validate_system_status(&status)?;
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
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for raw in lookup {
            let Ok(book) = serde_json::from_value::<BookShape>(raw.clone()) else {
                warn!("Skipping malformed Chaptarr lookup result");
                continue;
            };
            if !book_identity_complete(&book)
                || junk_title(&book.title)
                || !search_shape_matches_format(&book, self.format)
            {
                debug!(title = %book.title, "Skipping incomplete or incompatible Chaptarr lookup result");
                continue;
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
            if !seen.insert(key) {
                continue;
            }
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
            items.push(ChaptarrItem {
                book,
                display_title,
                cover,
                existing_book_id,
            });
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
        debug!(count = items.len(), "Chaptarr search complete");
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

fn resolve_profile(
    profiles: &[Profile],
    format: ChaptarrFormat,
    metadata: bool,
    configured: Option<&str>,
) -> Result<i32> {
    let expected_number = match format {
        ChaptarrFormat::Ebook => 2,
        ChaptarrFormat::Audiobook => 1,
    };
    let expected_string = format_name(format);
    let matches: Vec<_> = profiles
        .iter()
        .filter(|p| {
            if metadata {
                p.profile_type.as_i64() == Some(expected_number)
            } else {
                p.profile_type.as_str() == Some(expected_string)
            }
        })
        .filter(|p| configured.is_none_or(|name| p.name == name))
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].id);
    }
    let kind = if metadata { "metadata" } else { "quality" };
    let available = profiles
        .iter()
        .filter(|p| {
            if metadata {
                p.profile_type.as_i64() == Some(expected_number)
            } else {
                p.profile_type.as_str() == Some(expected_string)
            }
        })
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "Chaptarr {expected_string} {kind} profile selection is ambiguous. Configure an exact name; available: [{available}]"
    )
}

fn resolve_root(
    roots: &[RootFolder],
    format: ChaptarrFormat,
    configured: Option<&str>,
) -> Result<String> {
    let accessible: Vec<_> = roots.iter().filter(|root| root.accessible).collect();
    let matches: Vec<_> = if let Some(value) = configured {
        accessible
            .iter()
            .copied()
            .filter(|root| root.path == value || root.name == value)
            .collect()
    } else {
        let has_format_discriminators = accessible.iter().any(|root| {
            root.ebook
                || root.audiobook
                || root.is_effective_default_ebook
                || root.is_effective_default_audiobook
        });
        let mut inferred: Vec<_> = accessible
            .iter()
            .copied()
            .filter(|root| {
                if has_format_discriminators {
                    match format {
                        ChaptarrFormat::Ebook => root.ebook || root.is_effective_default_ebook,
                        ChaptarrFormat::Audiobook => {
                            root.audiobook || root.is_effective_default_audiobook
                        }
                    }
                } else {
                    let label = normalize(&format!("{} {}", root.name, root.path));
                    let audio = label.contains("audiobook") || label.contains("audio book");
                    match format {
                        ChaptarrFormat::Ebook => !audio && label.contains("book"),
                        ChaptarrFormat::Audiobook => audio,
                    }
                }
            })
            .collect();
        if inferred.is_empty() && accessible.len() == 1 {
            inferred.push(accessible[0]);
        }
        inferred
    };
    if matches.len() == 1 {
        return Ok(matches[0].path.clone());
    }
    let available = accessible
        .iter()
        .map(|r| r.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "Chaptarr {} root-folder selection is ambiguous. Configure an exact path or name; accessible: [{available}]",
        format_name(format)
    )
}

fn format_name(format: ChaptarrFormat) -> &'static str {
    match format {
        ChaptarrFormat::Ebook => "ebook",
        ChaptarrFormat::Audiobook => "audiobook",
    }
}

fn validate_system_status(status: &SystemStatus) -> Result<()> {
    if !status.app_name.eq_ignore_ascii_case("Chaptarr") {
        let identity = if status.app_name.trim().is_empty() {
            "missing appName"
        } else {
            status.app_name.as_str()
        };
        bail!("Configured Chaptarr URL returned an invalid identity ({identity})");
    }
    if status.version.trim().is_empty() {
        bail!("Configured Chaptarr URL did not report a version");
    }
    if !status.version.starts_with(TESTED_CHAPTARR_VERSION) {
        warn!(
            version = %status.version,
            tested = TESTED_CHAPTARR_VERSION,
            "Chaptarr version is outside the tested compatibility line"
        );
    }
    Ok(())
}

fn book_identity_complete(book: &BookShape) -> bool {
    !book.title.trim().is_empty()
        && !book.foreign_book_id.trim().is_empty()
        && !book.author.author_name.trim().is_empty()
        && !book.author.foreign_author_id.trim().is_empty()
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

fn null_if_empty(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        Value::String(value.to_string())
    }
}

fn open_library_work_search(book: &BookShape) -> String {
    let mut url = reqwest::Url::parse("https://openlibrary.org/search")
        .expect("the hard-coded Open Library URL must be valid");
    url.query_pairs_mut()
        .append_pair("q", &format!("{} {}", book.title, book.author.author_name));
    url.into()
}

fn positive_id(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64().filter(|id| *id > 0),
        Some(Value::String(s)) => s.parse::<i64>().ok().filter(|id| *id > 0),
        _ => None,
    }
}

fn string_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn parse_book(value: &Value) -> Option<BookShape> {
    serde_json::from_value(value.clone()).ok()
}

fn local_row_matches_format(value: &Value, format: ChaptarrFormat) -> bool {
    parse_book(value).is_some_and(|book| {
        !book.media_type.is_empty() && book.media_type.eq_ignore_ascii_case(format_name(format))
    })
}

fn local_row_matches_item(value: &Value, format: ChaptarrFormat, selected: &BookShape) -> bool {
    if !local_row_matches_format(value, format) {
        return false;
    }
    let Some(local) = parse_book(value) else {
        return false;
    };
    if !selected.foreign_book_id.is_empty() && !local.foreign_book_id.is_empty() {
        local.foreign_book_id == selected.foreign_book_id
    } else {
        title_match_tier(&local.title, &selected.title) > 0
    }
}

fn search_shape_matches_format(book: &BookShape, format: ChaptarrFormat) -> bool {
    if !book.media_type.is_empty() {
        return book.media_type.eq_ignore_ascii_case(format_name(format));
    }
    let flags: Vec<_> = book.editions.iter().filter_map(|e| e.is_ebook).collect();
    flags.is_empty()
        || flags
            .into_iter()
            .any(|v| v == (format == ChaptarrFormat::Ebook))
}

fn format_is_monitored(value: &Value, format: ChaptarrFormat) -> bool {
    let Some(book) = parse_book(value) else {
        return false;
    };
    if !book.media_type.eq_ignore_ascii_case(format_name(format)) {
        return false;
    }
    let format_flag = match format {
        ChaptarrFormat::Ebook => book.ebook_monitored,
        ChaptarrFormat::Audiobook => book.audiobook_monitored,
    };
    format_flag || book.monitored
}

fn format_state(value: &Value, format: ChaptarrFormat) -> FormatState {
    let Some(book) = parse_book(value) else {
        return FormatState::Missing;
    };
    if !book.media_type.eq_ignore_ascii_case(format_name(format)) {
        return FormatState::Missing;
    }
    let format_files = match format {
        ChaptarrFormat::Ebook => book.ebook_statistics.book_file_count,
        ChaptarrFormat::Audiobook => book.audiobook_statistics.book_file_count,
    };
    let files = format_files.max(book.statistics.book_file_count);
    if files > 0 || book.has_files {
        FormatState::Available
    } else if format_is_monitored(value, format) || book.grabbed {
        FormatState::Processing
    } else {
        FormatState::Missing
    }
}

fn preferred_book(rows: &[Value], format: ChaptarrFormat, selected: &BookShape) -> Option<Value> {
    let mut candidates: Vec<_> = rows
        .iter()
        .filter(|row| local_row_matches_item(row, format, selected))
        .cloned()
        .collect();
    candidates.sort_by_key(|row| {
        let tier = title_match_tier(string_at(row, "title"), &selected.title);
        let complete = book_complete(row);
        let shape = parse_book(row).unwrap_or_default();
        (
            tier,
            complete,
            shape.ratings.popularity.to_bits(),
            shape.ratings.votes,
            shape.release_date,
        )
    });
    candidates.pop()
}

fn title_match_tier(candidate: &str, requested: &str) -> u8 {
    if candidate.trim().is_empty() || requested.trim().is_empty() {
        return 0;
    }
    if normalize(candidate) == normalize(requested) {
        return 2;
    }

    fn is_subtitle_variant(longer: &str, shorter: &str) -> bool {
        let longer = longer.trim().to_lowercase();
        let shorter = shorter.trim().to_lowercase();
        longer.strip_prefix(&shorter).is_some_and(|suffix| {
            matches!(
                suffix.trim_start().chars().next(),
                Some(':' | '-' | '—' | '(')
            )
        })
    }

    u8::from(is_subtitle_variant(candidate, requested) || is_subtitle_variant(requested, candidate))
}

fn book_complete(value: &Value) -> bool {
    let Some(book) = parse_book(value) else {
        return false;
    };
    book.release_date.is_some()
        && !book.images.is_empty()
        && !book.foreign_edition_id.is_empty()
        && !book.foreign_edition_id.starts_with("default-")
}

fn needs_author_refresh(book: Option<&Value>) -> bool {
    book.is_some_and(|row| !book_complete(row))
}

fn absolute_cover(book: &BookShape, format: ChaptarrFormat) -> Option<String> {
    let expected_ebook = format == ChaptarrFormat::Ebook;
    book.images
        .iter()
        .filter(|i| i.cover_type == "cover")
        .chain(book.images.iter())
        .chain(
            book.editions
                .iter()
                .filter(|edition| edition.is_ebook.is_none_or(|value| value == expected_ebook))
                .flat_map(|edition| edition.images.iter()),
        )
        .map(|i| i.url.as_str())
        .chain(std::iter::once(book.remote_cover.as_str()))
        .find(|url| url.starts_with("https://"))
        .map(str::to_owned)
}

fn public_identifier_cover(book: &BookShape, format: ChaptarrFormat) -> Option<String> {
    let expected_ebook = format == ChaptarrFormat::Ebook;
    for edition in book
        .editions
        .iter()
        .filter(|edition| edition.is_ebook.is_none_or(|value| value == expected_ebook))
    {
        if let Some(isbn) = edition
            .isbn13
            .as_deref()
            .map(|s| s.chars().filter(char::is_ascii_digit).collect::<String>())
            .filter(|s| s.len() == 13)
        {
            return Some(format!(
                "https://covers.openlibrary.org/b/isbn/{isbn}-L.jpg?default=false"
            ));
        }
    }
    None
}

fn normalize(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn search_rank(title: &str, query: &str) -> (u8, Reverse<usize>) {
    let title = normalize(title);
    let rank = if title == query {
        3
    } else if title.starts_with(query) {
        2
    } else if title.contains(query) {
        1
    } else {
        0
    };
    (rank, Reverse(title.len().abs_diff(query.len())))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!(
            "{}…",
            result
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        result
    }
}

fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn junk_title(title: &str) -> bool {
    const PHRASES: &[&str] = &[
        "study guide",
        "sparknotes",
        "cliffsnotes",
        "summary and analysis",
        "summary of",
        "reader's companion",
        "unauthorized companion",
        "unofficial companion",
        "conversation starters",
        "discussion questions",
        "reading guide",
        "lesson plans",
        "key takeaways",
        "deep analysis",
    ];
    let title = title.to_lowercase();
    PHRASES.iter().any(|phrase| title.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const LOOKUP: &str = include_str!("../../tests/fixtures/chaptarr/lookup.json");
    const AUTHOR: &str = include_str!("../../tests/fixtures/chaptarr/author.json");
    const AVAILABLE: &str = include_str!("../../tests/fixtures/chaptarr/book_available.json");
    const PROCESSING: &str = include_str!("../../tests/fixtures/chaptarr/book_processing.json");
    const UNMONITORED: &str = include_str!("../../tests/fixtures/chaptarr/book_unmonitored.json");
    const PLACEHOLDER: &str = include_str!("../../tests/fixtures/chaptarr/book_placeholder.json");
    const QUALITY: &str = include_str!("../../tests/fixtures/chaptarr/quality_profiles.json");
    const METADATA: &str = include_str!("../../tests/fixtures/chaptarr/metadata_profiles.json");
    const ROOTS: &str = include_str!("../../tests/fixtures/chaptarr/root_folders.json");
    const LIVE_ROOTS: &str = include_str!("../../tests/fixtures/chaptarr/root_folders_nested.json");
    const STATUS: &str = include_str!("../../tests/fixtures/chaptarr/system_status.json");
    const OPEN_LIBRARY: &str =
        include_str!("../../tests/fixtures/chaptarr/openlibrary_search.json");
    const POST_BOOK_RESPONSE: &str =
        include_str!("../../tests/fixtures/chaptarr/post_book_response.json");
    const PUT_MONITOR_RESPONSE: &str =
        include_str!("../../tests/fixtures/chaptarr/put_monitor_response.json");
    const COMMAND_RESPONSE: &str =
        include_str!("../../tests/fixtures/chaptarr/command_response.json");

    fn backend(format: ChaptarrFormat) -> Chaptarr {
        Chaptarr {
            client: reqwest::Client::new(),
            base_url: "http://chaptarr.test/api/v1".into(),
            api_key: "test-only".into(),
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

    fn selected_book(title: &str, foreign_book_id: &str) -> BookShape {
        BookShape {
            title: title.into(),
            foreign_book_id: foreign_book_id.into(),
            ..BookShape::default()
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
    fn positive_ids_reject_placeholders() {
        assert_eq!(positive_id(Some(&json!("42"))), Some(42));
        assert_eq!(positive_id(Some(&json!(0))), None);
        assert_eq!(positive_id(Some(&json!("0"))), None);
    }

    #[test]
    fn lookup_fixture_preserves_format_local_ids_and_cover() {
        let rows: Vec<Value> = serde_json::from_str(LOOKUP).unwrap();
        let book: BookShape = serde_json::from_value(rows[0].clone()).unwrap();
        assert!(book_identity_complete(&book));
        assert_eq!(
            book.local_ebook_books
                .iter()
                .find_map(|row| positive_id(Some(&row.id))),
            Some(4101)
        );
        assert_eq!(
            book.local_audiobook_books
                .iter()
                .find_map(|row| positive_id(Some(&row.id))),
            Some(5101)
        );
        assert_eq!(
            absolute_cover(&book, ChaptarrFormat::Ebook).as_deref(),
            Some("https://covers.example.test/clockwork-orchard-ebook.jpg")
        );
        let junk: BookShape = serde_json::from_value(rows[2].clone()).unwrap();
        assert!(junk_title(&junk.title));
    }

    #[test]
    fn fixture_profiles_and_roots_resolve_exactly() {
        let quality: Vec<Profile> = serde_json::from_str(QUALITY).unwrap();
        let metadata: Vec<Profile> = serde_json::from_str(METADATA).unwrap();
        let roots: Vec<RootFolder> = serde_json::from_str(ROOTS).unwrap();
        assert_eq!(
            resolve_profile(&quality, ChaptarrFormat::Ebook, false, None).unwrap(),
            11
        );
        assert_eq!(
            resolve_profile(&metadata, ChaptarrFormat::Audiobook, true, None).unwrap(),
            22
        );
        assert_eq!(
            resolve_root(&roots, ChaptarrFormat::Ebook, Some("/library/ebooks")).unwrap(),
            "/library/ebooks"
        );
        assert_eq!(
            resolve_root(&roots, ChaptarrFormat::Ebook, None).unwrap(),
            "/library/ebooks"
        );
        assert_eq!(
            resolve_root(&roots, ChaptarrFormat::Audiobook, None).unwrap(),
            "/library/audiobooks"
        );
    }

    #[test]
    fn nested_root_settings_are_not_format_flags() {
        let roots: Vec<RootFolder> = serde_json::from_str(LIVE_ROOTS).unwrap();
        assert!(roots.iter().all(|root| !root.ebook && !root.audiobook));
        assert_eq!(
            resolve_root(&roots, ChaptarrFormat::Ebook, Some("/library/ebooks")).unwrap(),
            "/library/ebooks"
        );
        assert_eq!(
            resolve_root(&roots, ChaptarrFormat::Audiobook, None).unwrap(),
            "/library/audiobooks"
        );
    }

    #[test]
    fn ambiguous_root_inference_fails_closed() {
        let roots = vec![
            RootFolder {
                path: "/books/one".into(),
                name: "Books One".into(),
                accessible: true,
                ebook: false,
                audiobook: false,
                is_effective_default_ebook: false,
                is_effective_default_audiobook: false,
            },
            RootFolder {
                path: "/books/two".into(),
                name: "Books Two".into(),
                accessible: true,
                ebook: false,
                audiobook: false,
                is_effective_default_ebook: false,
                is_effective_default_audiobook: false,
            },
        ];
        assert!(resolve_root(&roots, ChaptarrFormat::Ebook, None).is_err());
    }

    #[test]
    fn system_status_fixture_identifies_tested_chaptarr() {
        let status: SystemStatus = serde_json::from_str(STATUS).unwrap();
        assert_eq!(status.app_name, "Chaptarr");
        assert!(status.version.starts_with(TESTED_CHAPTARR_VERSION));
        validate_system_status(&status).unwrap();
        assert!(validate_system_status(&SystemStatus::default()).is_err());
        assert!(
            validate_system_status(&SystemStatus {
                app_name: "Chaptarr".into(),
                version: String::new(),
            })
            .is_err()
        );
    }

    #[test]
    fn open_library_fixture_maps_only_exact_title_author_keys() {
        let response: OpenLibraryResponse = serde_json::from_str(OPEN_LIBRARY).unwrap();
        let covers = open_library_cover_map(response);
        assert_eq!(
            covers
                .get(&(normalize("The Clockwork Orchard"), normalize("Mara Vale"),))
                .map(String::as_str),
            Some("https://covers.openlibrary.org/b/id/990000001-L.jpg?default=false")
        );
        assert!(
            !covers.contains_key(&(normalize("The Clockwork Orchard"), normalize("Rowan Pike"),))
        );
    }

    #[test]
    fn mutation_acknowledgement_fixtures_remain_tolerant_json() {
        let post: Value = serde_json::from_str(POST_BOOK_RESPONSE).unwrap();
        let monitor: Value = serde_json::from_str(PUT_MONITOR_RESPONSE).unwrap();
        let command: Value = serde_json::from_str(COMMAND_RESPONSE).unwrap();
        assert!(post.is_object());
        assert!(monitor.is_object() || monitor.is_null());
        assert!(command.is_object());
    }

    #[test]
    fn optional_null_fields_do_not_discard_a_valid_identity() {
        let book: BookShape = serde_json::from_value(json!({
            "title": "The Clockwork Orchard",
            "overview": null,
            "foreignBookId": "hc:work-1001",
            "remoteCover": null,
            "mediaType": null,
            "monitored": null,
            "author": {
                "id": null,
                "authorName": "Avery North",
                "foreignAuthorId": "hc:author-1001"
            },
            "images": null,
            "editions": null,
            "localEbookBooks": null,
            "localAudiobookBooks": null,
            "ebookStatistics": null,
            "audiobookStatistics": null,
            "statistics": null,
            "ratings": null
        }))
        .unwrap();
        assert!(book_identity_complete(&book));
        assert!(book.images.is_empty());
        assert_eq!(book.statistics.book_file_count, 0);
    }

    #[test]
    fn status_is_format_scoped() {
        let row = json!({"mediaType":"ebook","hasFiles":true,"ebookMonitored":true});
        assert_eq!(
            format_state(&row, ChaptarrFormat::Ebook),
            FormatState::Available
        );
        assert_eq!(
            format_state(&row, ChaptarrFormat::Audiobook),
            FormatState::Missing
        );
    }

    #[test]
    fn fixture_states_are_strictly_format_scoped() {
        let available: Value = serde_json::from_str(AVAILABLE).unwrap();
        let processing: Value = serde_json::from_str(PROCESSING).unwrap();
        let unmonitored: Value = serde_json::from_str(UNMONITORED).unwrap();
        assert_eq!(
            format_state(&available, ChaptarrFormat::Ebook),
            FormatState::Available
        );
        assert_eq!(
            format_state(&available, ChaptarrFormat::Audiobook),
            FormatState::Missing
        );
        assert_eq!(
            format_state(&processing, ChaptarrFormat::Audiobook),
            FormatState::Processing
        );
        assert_eq!(
            format_state(&unmonitored, ChaptarrFormat::Ebook),
            FormatState::Missing
        );
    }

    #[test]
    fn row_statistics_are_accepted_only_for_matching_format() {
        let row = json!({
            "title": "A Book",
            "mediaType": "ebook",
            "statistics": {"bookFileCount": 1}
        });
        assert_eq!(
            format_state(&row, ChaptarrFormat::Ebook),
            FormatState::Available
        );
        assert_eq!(
            format_state(&row, ChaptarrFormat::Audiobook),
            FormatState::Missing
        );
    }

    #[test]
    fn exact_title_beats_popular_subtitle() {
        let rows = vec![
            json!({"id":1,"title":"The Women: A Novel","mediaType":"ebook","ratings":{"popularity":99}}),
            json!({"id":2,"title":"The Women","mediaType":"ebook","ratings":{"popularity":1}}),
        ];
        assert_eq!(
            preferred_book(
                &rows,
                ChaptarrFormat::Ebook,
                &selected_book("The Women", ""),
            )
            .and_then(|v| positive_id(v.get("id"))),
            Some(2)
        );
    }

    #[test]
    fn unrelated_sibling_is_never_selected() {
        let rows = vec![
            json!({"id":1,"title":"A Wizard of Earthsea","mediaType":"ebook"}),
            json!({"id":2,"title":"The Tombs of Atuan","mediaType":"ebook"}),
        ];
        assert!(
            preferred_book(&rows, ChaptarrFormat::Ebook, &selected_book("Tehanu", ""),).is_none()
        );
    }

    #[test]
    fn plain_prefix_is_not_treated_as_a_subtitle() {
        let rows = vec![json!({
            "id": 1,
            "title": "Dune Messiah",
            "mediaType": "ebook"
        })];
        assert!(
            preferred_book(&rows, ChaptarrFormat::Ebook, &selected_book("Dune", ""),).is_none()
        );
        assert_eq!(title_match_tier("The Women: A Novel", "The Women"), 1);
    }

    #[test]
    fn cross_format_row_is_never_selected() {
        let rows = vec![
            json!({"id":1,"title":"The Clockwork Orchard","mediaType":"ebook"}),
            json!({"id":2,"title":"The Clockwork Orchard","mediaType":"audiobook"}),
        ];
        assert_eq!(
            preferred_book(
                &rows,
                ChaptarrFormat::Audiobook,
                &selected_book("The Clockwork Orchard", ""),
            )
            .and_then(|row| positive_id(row.get("id"))),
            Some(2)
        );
    }

    #[test]
    fn lookup_local_id_must_match_selected_work_identity() {
        let selected = lookup_item();
        let wrong_work = json!({
            "id": 9999,
            "title": "Another Orchard",
            "foreignBookId": "hc:work-other",
            "mediaType": "ebook"
        });
        assert!(!local_row_matches_item(
            &wrong_work,
            ChaptarrFormat::Ebook,
            &selected.book,
        ));

        let same_title_wrong_id = json!({
            "id": 9998,
            "title": "The Clockwork Orchard",
            "foreignBookId": "hc:work-other",
            "mediaType": "ebook"
        });
        assert!(!local_row_matches_item(
            &same_title_wrong_id,
            ChaptarrFormat::Ebook,
            &selected.book,
        ));
        assert!(
            preferred_book(
                &[same_title_wrong_id],
                ChaptarrFormat::Ebook,
                &selected.book,
            )
            .is_none()
        );
    }

    #[test]
    fn placeholder_is_not_complete() {
        assert!(!book_complete(
            &json!({"releaseDate":"2020-01-01","images":[{"url":"x"}],"foreignEditionId":"default-1"})
        ));
        assert!(book_complete(
            &json!({"releaseDate":"2020-01-01","images":[{"url":"x"}],"foreignEditionId":"hc:1"})
        ));
    }

    #[test]
    fn placeholder_fixture_is_not_complete() {
        let placeholder: Value = serde_json::from_str(PLACEHOLDER).unwrap();
        assert!(!book_complete(&placeholder));
        assert!(needs_author_refresh(Some(&placeholder)));
        assert!(!needs_author_refresh(None));
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

    #[tokio::test]
    async fn search_and_confirmation_use_only_get_requests() {
        let lookup = json!([{
            "title": "The Clockwork Orchard",
            "overview": "A synthetic test book.",
            "foreignBookId": "hc:work-1001",
            "author": {
                "id": 0,
                "authorName": "Mara Vale",
                "foreignAuthorId": "hc:author-1001"
            },
            "editions": [{"isEbook": true}]
        }]);
        let (base_url, requests, server) =
            mock_api(vec![lookup.to_string(), "[]".to_string()]).await;
        let mut chaptarr = backend(ChaptarrFormat::Ebook);
        chaptarr.base_url = base_url;
        chaptarr.client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();

        let results = chaptarr.search("Clockwork Orchard").await.unwrap();
        assert_eq!(results.len(), 1);
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

    #[test]
    fn html_is_removed_for_discord() {
        assert_eq!(
            strip_html("A <i>good</i>&nbsp;book &amp; story"),
            "A good book & story"
        );
    }
}
