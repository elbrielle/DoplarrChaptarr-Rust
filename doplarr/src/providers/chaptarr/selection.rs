//! Pure compatibility, matching, and display helpers for Chaptarr data.

use super::models::{BookShape, OpenLibraryResponse, Profile, RootFolder, SystemStatus};
use crate::config::ChaptarrFormat;
use anyhow::{Result, bail};
use serde_json::Value;
use std::{cmp::Reverse, collections::HashMap};
use tracing::warn;

const TESTED_CHAPTARR_VERSION: &str = "0.9.720";

pub(super) fn version_is_tested(version: &str) -> bool {
    version.starts_with(TESTED_CHAPTARR_VERSION)
}

pub(super) type CoverMap = HashMap<(String, String), String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormatState {
    Available,
    Processing,
    Missing,
}

pub(super) fn open_library_cover_map(result: OpenLibraryResponse) -> CoverMap {
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

pub(super) fn resolve_profile(
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

pub(super) fn resolve_root(
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

pub(super) fn format_name(format: ChaptarrFormat) -> &'static str {
    match format {
        ChaptarrFormat::Ebook => "ebook",
        ChaptarrFormat::Audiobook => "audiobook",
    }
}

pub(super) fn validate_system_status(status: &SystemStatus) -> Result<()> {
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
    if !version_is_tested(&status.version) {
        warn!(
            version = %status.version,
            tested = TESTED_CHAPTARR_VERSION,
            "Chaptarr version is outside the tested compatibility line"
        );
    }
    Ok(())
}

pub(super) fn book_identity_complete(book: &BookShape) -> bool {
    !book.title.trim().is_empty()
        && !book.foreign_book_id.trim().is_empty()
        && !book.author.author_name.trim().is_empty()
        && !book.author.foreign_author_id.trim().is_empty()
}

pub(super) fn null_if_empty(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        Value::String(value.to_string())
    }
}

pub(super) fn open_library_work_search(book: &BookShape) -> String {
    let mut url = reqwest::Url::parse("https://openlibrary.org/search")
        .expect("the hard-coded Open Library URL must be valid");
    url.query_pairs_mut()
        .append_pair("q", &format!("{} {}", book.title, book.author.author_name));
    url.into()
}

pub(super) fn positive_id(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n.as_i64().filter(|id| *id > 0),
        Some(Value::String(s)) => s.parse::<i64>().ok().filter(|id| *id > 0),
        _ => None,
    }
}

pub(super) fn string_at<'a>(value: &'a Value, key: &str) -> &'a str {
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

pub(super) fn local_row_matches_item(
    value: &Value,
    format: ChaptarrFormat,
    selected: &BookShape,
) -> bool {
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

/// Rank duplicate lookup projections without treating a projection as an
/// availability claim. Chaptarr can return an audiobook-shaped projection for
/// a work requested as an ebook, so even a score of zero remains searchable.
pub(super) fn search_format_affinity(book: &BookShape, format: ChaptarrFormat) -> u8 {
    if !book.media_type.is_empty() {
        return u8::from(book.media_type.eq_ignore_ascii_case(format_name(format))) * 3;
    }
    let flags: Vec<_> = book.editions.iter().filter_map(|e| e.is_ebook).collect();
    if flags.is_empty() {
        1
    } else if flags
        .into_iter()
        .any(|v| v == (format == ChaptarrFormat::Ebook))
    {
        2
    } else {
        0
    }
}

pub(super) fn format_is_monitored(value: &Value, format: ChaptarrFormat) -> bool {
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

pub(super) fn format_state(value: &Value, format: ChaptarrFormat) -> FormatState {
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

pub(super) fn preferred_book(
    rows: &[Value],
    format: ChaptarrFormat,
    selected: &BookShape,
) -> Option<Value> {
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

pub(super) fn title_match_tier(candidate: &str, requested: &str) -> u8 {
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

pub(super) fn book_complete(value: &Value) -> bool {
    let Some(book) = parse_book(value) else {
        return false;
    };
    book.release_date.is_some()
        && !book.images.is_empty()
        && !book.foreign_edition_id.is_empty()
        && !book.foreign_edition_id.starts_with("default-")
}

pub(super) fn needs_author_refresh(book: Option<&Value>) -> bool {
    book.is_some_and(|row| !book_complete(row))
}

pub(super) fn absolute_cover(book: &BookShape, format: ChaptarrFormat) -> Option<String> {
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

pub(super) fn public_identifier_cover(book: &BookShape, format: ChaptarrFormat) -> Option<String> {
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

pub(super) fn normalize(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn search_rank(title: &str, query: &str) -> (u8, Reverse<usize>) {
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

pub(super) fn truncate_chars(value: &str, limit: usize) -> String {
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

pub(super) fn strip_html(input: &str) -> String {
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

pub(super) fn junk_title(title: &str) -> bool {
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
    use serde_json::json;

    const LOOKUP: &str = include_str!("../../../tests/fixtures/chaptarr/lookup.json");
    const AVAILABLE: &str = include_str!("../../../tests/fixtures/chaptarr/book_available.json");
    const PROCESSING: &str = include_str!("../../../tests/fixtures/chaptarr/book_processing.json");
    const UNMONITORED: &str =
        include_str!("../../../tests/fixtures/chaptarr/book_unmonitored.json");
    const PLACEHOLDER: &str =
        include_str!("../../../tests/fixtures/chaptarr/book_placeholder.json");
    const QUALITY: &str = include_str!("../../../tests/fixtures/chaptarr/quality_profiles.json");
    const METADATA: &str = include_str!("../../../tests/fixtures/chaptarr/metadata_profiles.json");
    const ROOTS: &str = include_str!("../../../tests/fixtures/chaptarr/root_folders.json");
    const LIVE_ROOTS: &str =
        include_str!("../../../tests/fixtures/chaptarr/root_folders_nested.json");
    const STATUS: &str = include_str!("../../../tests/fixtures/chaptarr/system_status.json");
    const OPEN_LIBRARY: &str =
        include_str!("../../../tests/fixtures/chaptarr/openlibrary_search.json");
    const POST_BOOK_RESPONSE: &str =
        include_str!("../../../tests/fixtures/chaptarr/post_book_response.json");
    const PUT_MONITOR_RESPONSE: &str =
        include_str!("../../../tests/fixtures/chaptarr/put_monitor_response.json");
    const COMMAND_RESPONSE: &str =
        include_str!("../../../tests/fixtures/chaptarr/command_response.json");

    fn selected_book(title: &str, foreign_book_id: &str) -> BookShape {
        BookShape {
            title: title.into(),
            foreign_book_id: foreign_book_id.into(),
            ..BookShape::default()
        }
    }

    fn lookup_book() -> BookShape {
        let rows: Vec<Value> = serde_json::from_str(LOOKUP).unwrap();
        serde_json::from_value(rows[0].clone()).unwrap()
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
    fn lookup_projection_is_a_preference_not_an_exclusion() {
        let mut book = lookup_book();
        book.media_type = "audiobook".into();
        assert_eq!(search_format_affinity(&book, ChaptarrFormat::Ebook), 0);
        assert_eq!(search_format_affinity(&book, ChaptarrFormat::Audiobook), 3);

        book.media_type.clear();
        assert_eq!(search_format_affinity(&book, ChaptarrFormat::Ebook), 2);
        assert_eq!(search_format_affinity(&book, ChaptarrFormat::Audiobook), 2);
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
        let selected = lookup_book();
        let wrong_work = json!({
            "id": 9999,
            "title": "Another Orchard",
            "foreignBookId": "hc:work-other",
            "mediaType": "ebook"
        });
        assert!(!local_row_matches_item(
            &wrong_work,
            ChaptarrFormat::Ebook,
            &selected,
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
            &selected,
        ));
        assert!(
            preferred_book(&[same_title_wrong_id], ChaptarrFormat::Ebook, &selected,).is_none()
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
    fn html_is_removed_for_discord() {
        assert_eq!(
            strip_html("A <i>good</i>&nbsp;book &amp; story"),
            "A good book & story"
        );
    }
}
