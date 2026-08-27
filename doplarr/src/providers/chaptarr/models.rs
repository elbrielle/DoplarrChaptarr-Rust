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

/// On 0.9.936 the root-folder `ebook`/`audiobook` keys are nested settings
/// objects, present only when the root is configured for that format
/// (`RootFolderResource.cs:46-47,399-400`); pre-0.9.936 payloads used plain
/// booleans. Only a literal `true` sets this legacy flag — object presence is
/// not yet consumed for resolution, which still keys on explicit flags,
/// effective defaults, and name inference.
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

/// `url` is always rewritten to a relative proxied path
/// (`MediaCoverService.cs:405-414` registers `/MediaCoverProxy/...`); the
/// absolute upstream URL survives only on `remoteUrl` (`MediaCover.cs`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Image {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) url: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) cover_type: String,
    #[serde(default, deserialize_with = "null_default")]
    pub(super) remote_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Edition {
    #[serde(default, deserialize_with = "null_default")]
    pub(super) id: Value,
    /// Verbatim provider text ("Kindle Edition", "Hardcover", ...;
    /// `Edition.cs:41`) — display and logging only, never a discriminator.
    #[serde(default, deserialize_with = "null_default")]
    pub(super) format: String,
    /// Chaptarr's structured edition discriminator: 1=physical, 2=audio,
    /// 3=ebook (`Edition.cs:58`, `EditionResource.cs:48`; nullable, omitted
    /// when unset).
    #[serde(default)]
    pub(super) reading_format_id: Option<i64>,
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

/// Serializer traps: 0.9.936 omits null properties (`STJson.cs:27`),
/// `id: 0` (`RestResource.cs:7-8`), `grabbed: false`
/// (`BookResource.cs:87-88,236`), the `editions` key on every `/book`
/// response (`BookResource.cs:137-259` never assigns it), nullable
/// `readingFormatId`, and unconfigured nested root settings
/// (`RootFolderResource.cs:399-400`). Every model must deserialize those
/// key-absent shapes without error.
#[cfg(test)]
mod serializer_traps {
    use super::*;
    use serde_json::json;

    #[test]
    fn book_rows_tolerate_absent_id_grabbed_editions_and_monitor_gates() {
        let book: BookShape = serde_json::from_value(json!({
            "title": "Sparse Row",
            "foreignBookId": "gr:work-1",
            "mediaType": "ebook",
            "author": {"authorName": "Mara Vale", "foreignAuthorId": "gr:author-1"}
        }))
        .unwrap();
        assert!(book.editions.is_empty());
        assert!(!book.grabbed);
        assert!(!book.monitored && !book.ebook_monitored && !book.audiobook_monitored);
        assert!(book.release_date.is_none());
        assert!(book.images.is_empty());
        assert!(book.local_ebook_books.is_empty());
        assert_eq!(book.statistics.book_file_count, 0);
    }

    #[test]
    fn a_grabbed_key_in_input_is_still_parsed_as_data_not_required() {
        // `grabbed` is only ever emitted on the SignalR path
        // (BookController.cs:1997); its absence from REST rows is the norm.
        let with_key: BookShape = serde_json::from_value(json!({"grabbed": true})).unwrap();
        let without_key: BookShape = serde_json::from_value(json!({})).unwrap();
        assert!(with_key.grabbed);
        assert!(!without_key.grabbed);
    }

    #[test]
    fn editions_tolerate_absent_id_format_identity_and_flags() {
        let edition: Edition = serde_json::from_value(json!({
            "title": "Untyped projection"
        }))
        .unwrap();
        assert!(edition.format.is_empty());
        assert!(edition.is_ebook.is_none());
        assert!(edition.isbn13.is_none() && edition.asin.is_none());
        assert!(!edition.monitored);
        assert!(edition.foreign_edition_id.is_empty());
        let empty: Edition = serde_json::from_value(json!({})).unwrap();
        assert!(empty.title.is_empty());
    }

    #[test]
    fn root_folders_tolerate_absent_or_object_valued_nested_settings() {
        let bare: RootFolder = serde_json::from_value(json!({
            "path": "/library/ebooks"
        }))
        .unwrap();
        assert!(bare.accessible);
        assert!(!bare.ebook && !bare.audiobook);

        let configured: RootFolder = serde_json::from_value(json!({
            "path": "/library/ebooks",
            "folderType": 2,
            "ebook": {"writeAudioBookShelfMetadataJson": false, "tags": []}
        }))
        .unwrap();
        assert!(
            !configured.ebook,
            "a settings object is not the legacy bool flag"
        );
    }

    #[test]
    fn lookup_rows_tolerate_absent_local_book_ids() {
        let book: BookShape = serde_json::from_value(json!({
            "localEbookBooks": [{}],
            "localAudiobookBooks": [{"id": 5101}]
        }))
        .unwrap();
        assert!(book.local_ebook_books[0].id.is_null());
        assert_eq!(book.local_audiobook_books[0].id, json!(5101));
    }
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
