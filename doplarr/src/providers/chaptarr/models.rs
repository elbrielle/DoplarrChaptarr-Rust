//! Tolerant data models for the narrow Chaptarr API contract.

use serde::Deserialize;
use serde_json::Value;

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// Chaptarr has exposed the root-folder `ebook` and `audiobook` keys as both
/// booleans and nested settings objects. Only an explicit `true` is a format
/// discriminator; an object exists on both root types and must not select one.
fn bool_only<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Value>::deserialize(deserializer).map(|value| matches!(value, Some(Value::Bool(true))))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Profile {
    pub(super) id: i32,
    pub(super) name: String,
    #[serde(default)]
    pub(super) profile_type: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RootFolder {
    pub(super) path: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) name: String,
    #[serde(default = "default_true", deserialize_with = "null_default")]
    pub(super) accessible: bool,
    #[serde(default, deserialize_with = "bool_only")]
    pub(super) ebook: bool,
    #[serde(default, deserialize_with = "bool_only")]
    pub(super) audiobook: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) is_effective_default_ebook: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) is_effective_default_audiobook: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SearchAuthor {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) author_name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) foreign_author_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Image {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) url: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) cover_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Edition {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) id: Value,
    /// Chaptarr's authoritative edition discriminator (`ebook`, `audiobook`,
    /// or `physical`). Older/projection responses may omit it, in which case
    /// callers can fall back to `is_ebook` deliberately.
    #[serde(default, deserialize_with = "null_default")]
    pub(super) format: String,
    #[serde(default)]
    pub(super) is_ebook: Option<bool>,
    #[serde(default)]
    pub(super) isbn13: Option<String>,
    #[serde(default)]
    pub(super) asin: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) language: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) foreign_edition_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) images: Vec<Image>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LocalBook {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) id: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Statistics {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) book_file_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SystemStatus {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) app_name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) version: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ratings {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) popularity: f64,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) votes: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BookShape {
    #[serde(default, deserialize_with = "null_default")]
    #[serde(alias = "bookTitle")]
    pub(super) title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) overview: String,
    #[serde(default)]
    pub(super) release_date: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) foreign_book_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) foreign_edition_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) remote_cover: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) media_type: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) ebook_monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) audiobook_monitored: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) has_files: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) grabbed: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) author: SearchAuthor,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) images: Vec<Image>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) editions: Vec<Edition>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) local_ebook_books: Vec<LocalBook>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) local_audiobook_books: Vec<LocalBook>,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) ebook_statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) audiobook_statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) statistics: Statistics,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) ratings: Ratings,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OpenLibraryResponse {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) docs: Vec<OpenLibraryDoc>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct OpenLibraryDoc {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) title: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) author_name: Vec<String>,
    pub(super) cover_i: Option<i64>,
}
