//! Pure compatibility, matching, and display helpers for Chaptarr data.

use super::models::{BookShape, Edition, OpenLibraryResponse, Profile, RootFolder, SystemStatus};
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
    if !selected.foreign_book_id.trim().is_empty() {
        // Once lookup supplied a stable work id, never substitute a same-title
        // row whose id is missing or different. Title matching is only a
        // fallback for projections that genuinely have no work id.
        !local.foreign_book_id.trim().is_empty()
            && local.foreign_book_id == selected.foreign_book_id
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
    if book.editions.is_empty()
        || book
            .editions
            .iter()
            .all(|edition| edition.format.trim().is_empty() && edition.is_ebook.is_none())
    {
        1
    } else if book
        .editions
        .iter()
        .any(|edition| edition_projection_compatible(edition, format))
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
    book.monitored && format_flag
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

fn format_state_rank(state: FormatState) -> u8 {
    match state {
        FormatState::Available => 2,
        FormatState::Processing => 1,
        FormatState::Missing => 0,
    }
}

/// Chaptarr imports can leave duplicate local rows ("pockets") for the same
/// work, and availability or an in-flight grab may live on either twin. The
/// authoritative state for a request is therefore the strongest state across
/// every local row that matches the selected work, never a single row.
pub(super) fn format_state_across(
    rows: &[Value],
    format: ChaptarrFormat,
    selected: &BookShape,
) -> FormatState {
    rows.iter()
        .filter(|row| local_row_matches_item(row, format, selected))
        .map(|row| format_state(row, format))
        .max_by_key(|state| format_state_rank(*state))
        .unwrap_or(FormatState::Missing)
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

pub(super) fn parse_edition(value: &Value) -> Option<Edition> {
    serde_json::from_value(value.clone()).ok()
}

fn command_references_author(command: &Value, author_id: i64) -> Option<bool> {
    // Commands echo their payload under `body` on current builds; tolerate a
    // flattened shape as well.
    let scopes = [command.get("body"), Some(command)];
    let mut saw_scope = false;
    for scope in scopes.into_iter().flatten() {
        if let Some(id) = positive_id(scope.get("authorId")) {
            saw_scope = true;
            if id == author_id {
                return Some(true);
            }
        }
        if let Some(ids) = scope.get("authorIds").and_then(Value::as_array) {
            saw_scope = true;
            if ids
                .iter()
                .any(|id| positive_id(Some(id)) == Some(author_id))
            {
                return Some(true);
            }
        }
    }
    if saw_scope { Some(false) } else { None }
}

/// True while a queued or running Chaptarr command could still be mutating
/// this author's catalog: any command scoped to the author, or an unscoped
/// refresh-style command that may sweep every author.
pub(super) fn catalog_command_active(commands: &[Value], author_id: i64) -> bool {
    commands.iter().any(|command| {
        let status = string_at(command, "status").to_ascii_lowercase();
        if status != "queued" && status != "started" {
            return false;
        }
        match command_references_author(command, author_id) {
            Some(references) => references,
            None => {
                let name = {
                    let name = string_at(command, "name");
                    if name.is_empty() {
                        string_at(command, "commandName")
                    } else {
                        name
                    }
                }
                .to_ascii_lowercase();
                name.contains("refresh")
            }
        }
    })
}

/// Relevant identity and completeness fields for one catalog row. Watching
/// these fields prevents a row from looking settled merely because its id was
/// allocated before Chaptarr populated the rest of its metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BookRowFingerprint {
    id: i64,
    foreign_book_id: String,
    title: String,
    foreign_edition_id: String,
    media_type: String,
    release_date: String,
    image_count: usize,
    embedded_edition_count: usize,
    monitored: bool,
    ebook_monitored: bool,
    audiobook_monitored: bool,
}

/// Order-independent identity and readiness shape of an author's book list,
/// used to decide when a fresh catalog import has stopped adding, replacing,
/// or completing rows in place.
pub(super) fn book_list_fingerprint(rows: &[Value]) -> Vec<BookRowFingerprint> {
    let mut fingerprint: Vec<BookRowFingerprint> = rows
        .iter()
        .map(|row| {
            let book = parse_book(row).unwrap_or_default();
            BookRowFingerprint {
                id: positive_id(row.get("id")).unwrap_or_default(),
                foreign_book_id: book.foreign_book_id,
                title: book.title,
                foreign_edition_id: book.foreign_edition_id,
                media_type: book.media_type,
                release_date: book.release_date.unwrap_or_default(),
                image_count: book.images.len(),
                embedded_edition_count: book.editions.len(),
                monitored: book.monitored,
                ebook_monitored: book.ebook_monitored,
                audiobook_monitored: book.audiobook_monitored,
            }
        })
        .collect();
    fingerprint.sort();
    fingerprint
}

/// An edition can be mutated or accepted during read-back only when
/// Chaptarr's authoritative `format` is present and matches the request.
/// `isEbook=false` cannot distinguish an audiobook from a physical edition,
/// so the legacy boolean is never sufficient for a write decision.
pub(super) fn edition_usable(edition: &Edition, format: ChaptarrFormat) -> bool {
    let declared = edition.format.trim();
    !declared.is_empty() && declared.eq_ignore_ascii_case(format_name(format))
}

/// Lookup ranking and cover discovery are non-mutating and can safely tolerate
/// an old projection with no authoritative format. Explicit physical,
/// unrecognized, or opposite-format records are still excluded.
fn edition_projection_compatible(edition: &Edition, format: ChaptarrFormat) -> bool {
    let declared = edition.format.trim();
    if !declared.is_empty() {
        return declared.eq_ignore_ascii_case(format_name(format));
    }
    edition
        .is_ebook
        .is_none_or(|value| value == (format == ChaptarrFormat::Ebook))
}

pub(super) fn usable_edition_count(editions: &[Value], format: ChaptarrFormat) -> usize {
    editions
        .iter()
        .filter_map(parse_edition)
        .filter(|edition| edition_usable(edition, format))
        .count()
}

/// English is a ranking preference only, never a filter: the metadata server
/// returns heavily translation-padded edition lists, and the household's
/// libraries are English. A work with no English edition still resolves.
pub(super) fn is_english(language: &str) -> bool {
    matches!(
        normalize(language)
            .split_whitespace()
            .next()
            .unwrap_or_default(),
        "en" | "eng" | "english"
    )
}

/// Choose the single edition to monitor, mirroring a manual pick in the
/// Chaptarr UI. Only editions of the requested format are considered; among
/// those, reject companion/summary material and prefer an exact title before
/// considering the projected edition or language. This keeps a projected or
/// English summary from outranking the real work. Public identifiers remain a
/// final indexer-match tiebreaker.
pub(super) fn preferred_edition_index(
    editions: &[Value],
    format: ChaptarrFormat,
    selected: &BookShape,
) -> Option<usize> {
    editions
        .iter()
        .enumerate()
        .filter_map(|(index, value)| parse_edition(value).map(|edition| (index, edition)))
        .filter(|(_, edition)| edition_usable(edition, format))
        .max_by_key(|(index, edition)| {
            let title_rank = if edition.title.trim().is_empty() {
                1
            } else {
                title_match_tier(&edition.title, &selected.title)
            };
            (
                !junk_title(&edition.title),
                title_rank,
                !edition.foreign_edition_id.is_empty()
                    && edition.foreign_edition_id == selected.foreign_edition_id,
                is_english(&edition.language),
                edition.isbn13.is_some() || edition.asin.is_some(),
                Reverse(*index),
            )
        })
        .map(|(index, _)| index)
}

/// Pick which duplicate local pocket of the selected work to monitor. Import
/// bugs can leave twin rows for one `foreignBookId` whose edition sets are not
/// equivalent, so a row that actually carries usable requested-format editions
/// beats every row that does not; the usual completeness/popularity ordering
/// only breaks ties. Returns an index into `rows`.
pub(super) fn preferred_pocket(
    rows: &[(Value, Vec<Value>)],
    format: ChaptarrFormat,
    selected: &BookShape,
) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, (row, _))| local_row_matches_item(row, format, selected))
        .max_by_key(|(index, (row, editions))| {
            let usable = usable_edition_count(editions, format);
            let shape = parse_book(row).unwrap_or_default();
            (
                usable > 0,
                title_match_tier(string_at(row, "title"), &selected.title),
                book_complete(row),
                usable,
                shape.ratings.popularity.to_bits(),
                shape.ratings.votes,
                shape.release_date,
                Reverse(*index),
            )
        })
        .map(|(index, _)| index)
}

/// Exactly-one-monitored-edition read-back: returns the lone monitored
/// edition only when a single one exists and it does not contradict the
/// requested format. Anything else means the request must not proceed.
pub(super) fn sole_monitored_edition(
    editions: &[Value],
    format: ChaptarrFormat,
) -> Option<Edition> {
    let mut monitored = Vec::new();
    for value in editions {
        match value.get("monitored") {
            None | Some(Value::Null | Value::Bool(false)) => continue,
            Some(Value::Bool(true)) => {
                // A malformed monitored row cannot be silently discarded: it
                // may be a second selected edition or the only selected one.
                let edition = parse_edition(value)?;
                if !edition.monitored {
                    return None;
                }
                monitored.push(edition);
            }
            Some(_) => return None,
        }
    }
    match <[Edition; 1]>::try_from(monitored) {
        Ok([edition]) if edition_usable(&edition, format) => Some(edition),
        _ => None,
    }
}

/// True when the two edition records identify the same edition. Local ids are
/// authoritative; the foreign edition id is the fallback. Unidentifiable
/// records never match, so verification fails closed.
pub(super) fn same_edition(left: &Edition, right: &Edition) -> bool {
    if let (Some(a), Some(b)) = (positive_id(Some(&left.id)), positive_id(Some(&right.id))) {
        return a == b;
    }
    !left.foreign_edition_id.trim().is_empty()
        && left.foreign_edition_id == right.foreign_edition_id
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
    book.images
        .iter()
        .filter(|i| i.cover_type == "cover")
        .chain(book.images.iter())
        .chain(
            book.editions
                .iter()
                .filter(|edition| edition_projection_compatible(edition, format))
                .flat_map(|edition| edition.images.iter()),
        )
        .map(|i| i.url.as_str())
        .chain(std::iter::once(book.remote_cover.as_str()))
        .find(|url| url.starts_with("https://"))
        .map(str::to_owned)
}

pub(super) fn public_identifier_cover(book: &BookShape, format: ChaptarrFormat) -> Option<String> {
    for edition in book
        .editions
        .iter()
        .filter(|edition| edition_projection_compatible(edition, format))
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

/// Conservatively identify search results that clearly represent several
/// books rather than one work. A plain story "collection" is intentionally
/// not enough; the title must carry a strong multi-book signal.
pub(super) fn clear_multi_book_result_title(title: &str) -> bool {
    let normalized = normalize(title);
    let words: Vec<_> = normalized.split_whitespace().collect();
    let last = words.last().copied().unwrap_or_default();

    if matches!(last, "bundle" | "bundles")
        || words.contains(&"omnibus")
        || words
            .windows(2)
            .any(|pair| matches!(pair, ["box" | "boxed", "set" | "sets"]))
        || last == "trilogy"
        || (last == "series" && words.contains(&"complete"))
    {
        return true;
    }

    fn is_multi_count(word: &str) -> bool {
        word.parse::<u16>().is_ok_and(|count| count > 1)
            || matches!(
                word,
                "two"
                    | "three"
                    | "four"
                    | "five"
                    | "six"
                    | "seven"
                    | "eight"
                    | "nine"
                    | "ten"
                    | "eleven"
                    | "twelve"
            )
    }

    words.windows(3).any(|window| {
        is_multi_count(window[0])
            && matches!(window[1], "book" | "books")
            && matches!(window[2], "collection" | "set")
    }) || words.windows(4).any(|window| {
        window[0] == "collection"
            && window[1] == "of"
            && is_multi_count(window[2])
            && matches!(window[3], "book" | "books")
    })
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
    const EDITION_FORMATS: &str =
        include_str!("../../../tests/fixtures/chaptarr/edition_formats.json");

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
    fn monitoring_readiness_requires_both_generic_and_format_flags() {
        let ready = json!({
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true
        });
        let generic_only = json!({
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": false
        });
        let format_only = json!({
            "mediaType": "ebook",
            "monitored": false,
            "ebookMonitored": true
        });

        assert!(format_is_monitored(&ready, ChaptarrFormat::Ebook));
        assert!(!format_is_monitored(&generic_only, ChaptarrFormat::Ebook));
        assert!(!format_is_monitored(&format_only, ChaptarrFormat::Ebook));
        assert_eq!(
            format_state(&generic_only, ChaptarrFormat::Ebook),
            FormatState::Missing,
            "a generic-only partial mutation must remain repairable"
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

        let same_title_missing_id = json!({
            "id": 9997,
            "title": "The Clockwork Orchard",
            "mediaType": "ebook"
        });
        assert!(!local_row_matches_item(
            &same_title_missing_id,
            ChaptarrFormat::Ebook,
            &selected,
        ));
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

    #[test]
    fn duplicate_pockets_resolve_to_the_row_with_usable_editions() {
        // The live incident shape: two local rows for one foreignBookId. The
        // twin with only an audiobook-slanted edition must lose an ebook
        // request to the row that carries real ebook editions.
        let selected = selected_book("Royal Assassin", "hc:54244");
        let starved = json!({
            "id": 40717,
            "title": "Royal Assassin",
            "foreignBookId": "hc:54244",
            "mediaType": "ebook",
            "releaseDate": "1996-03-01",
            "foreignEditionId": "hc:audio-slanted",
            "images": [{"url": "https://covers.example.test/x.jpg"}],
            "ratings": {"popularity": 99, "votes": 9000}
        });
        let stocked = json!({
            "id": 40510,
            "title": "Royal Assassin",
            "foreignBookId": "hc:54244",
            "mediaType": "ebook",
            "releaseDate": "1996-03-01",
            "foreignEditionId": "hc:eng-ebook",
            "images": [{"url": "https://covers.example.test/y.jpg"}],
            "ratings": {"popularity": 1, "votes": 1}
        });
        let rows = vec![
            (
                starved,
                vec![json!({
                    "id": 1,
                    "format": "audiobook",
                    "isEbook": false,
                    "language": "eng"
                })],
            ),
            (
                stocked,
                vec![
                    json!({"id": 2, "format": "ebook", "isEbook": true, "language": "eng"}),
                    json!({"id": 3, "format": "ebook", "isEbook": true, "language": "fre"}),
                ],
            ),
        ];
        assert_eq!(
            preferred_pocket(&rows, ChaptarrFormat::Ebook, &selected),
            Some(1)
        );
        // For an audiobook request the starved row is the only usable pocket.
        assert_eq!(
            preferred_pocket(&rows, ChaptarrFormat::Audiobook, &selected),
            None,
            "cross-format rows must never match"
        );
    }

    #[test]
    fn pocket_without_usable_editions_is_still_reported_for_diagnosis() {
        let selected = selected_book("The Devils", "hc:devils");
        let pruned = json!({
            "id": 41000,
            "title": "The Devils",
            "foreignBookId": "hc:devils",
            "mediaType": "ebook"
        });
        let rows = vec![(pruned, Vec::new())];
        // The row still resolves (so the caller can name the problem), but no
        // edition is choosable, which must fail the request loudly.
        assert_eq!(
            preferred_pocket(&rows, ChaptarrFormat::Ebook, &selected),
            Some(0)
        );
        assert_eq!(
            preferred_edition_index(&[], ChaptarrFormat::Ebook, &selected),
            None
        );
    }

    #[test]
    fn edition_choice_prefers_explicit_format_english_and_exact_title() {
        let selected = lookup_book();
        let editions = vec![
            json!({"id": 1, "format": "ebook", "isEbook": true, "language": "ger", "title": "Die Uhrwerk-Plantage"}),
            json!({"id": 2, "format": "ebook", "isEbook": true, "language": "eng", "title": "The Clockwork Orchard", "isbn13": "9780000000002"}),
            json!({"id": 3, "format": "audiobook", "isEbook": false, "language": "eng", "title": "The Clockwork Orchard"}),
            json!({"id": 4, "language": "eng", "title": "The Clockwork Orchard"}),
        ];
        assert_eq!(
            preferred_edition_index(&editions, ChaptarrFormat::Ebook, &selected),
            Some(1)
        );
        assert_eq!(
            preferred_edition_index(&editions, ChaptarrFormat::Audiobook, &selected),
            Some(2)
        );
    }

    #[test]
    fn authoritative_edition_format_rejects_physical_and_bad_values() {
        let editions: Vec<Value> = serde_json::from_str(EDITION_FORMATS).unwrap();
        assert_eq!(usable_edition_count(&editions, ChaptarrFormat::Ebook), 1);
        assert_eq!(
            usable_edition_count(&editions, ChaptarrFormat::Audiobook),
            1,
            "a live-shaped physical edition must not count as an audiobook"
        );

        let physical_claiming_ebook = parse_edition(&json!({
            "id": 1,
            "format": "physical",
            "isEbook": true
        }))
        .unwrap();
        assert!(!edition_usable(
            &physical_claiming_ebook,
            ChaptarrFormat::Ebook
        ));
        assert!(!edition_usable(
            &physical_claiming_ebook,
            ChaptarrFormat::Audiobook
        ));

        let ebook_with_stale_legacy_flag = parse_edition(&json!({
            "id": 2,
            "format": "EBOOK",
            "isEbook": false
        }))
        .unwrap();
        assert!(edition_usable(
            &ebook_with_stale_legacy_flag,
            ChaptarrFormat::Ebook
        ));

        let unknown = parse_edition(&json!({
            "id": 3,
            "format": "comic",
            "isEbook": true
        }))
        .unwrap();
        assert!(!edition_usable(&unknown, ChaptarrFormat::Ebook));

        let physical_projection: BookShape = serde_json::from_value(json!({
            "editions": [{"id": 30, "format": "physical", "isEbook": false}]
        }))
        .unwrap();
        assert_eq!(
            search_format_affinity(&physical_projection, ChaptarrFormat::Audiobook),
            0,
            "physical-only lookup data must not create audiobook affinity"
        );

        let legacy_ebook = parse_edition(&json!({"id": 4, "isEbook": true})).unwrap();
        let legacy_audio = parse_edition(&json!({"id": 5, "isEbook": false})).unwrap();
        let untyped = parse_edition(&json!({"id": 6})).unwrap();
        assert!(!edition_usable(&legacy_ebook, ChaptarrFormat::Ebook));
        assert!(!edition_usable(&legacy_audio, ChaptarrFormat::Audiobook));
        assert!(!edition_usable(&untyped, ChaptarrFormat::Ebook));
        assert!(!edition_usable(&untyped, ChaptarrFormat::Audiobook));
        assert!(edition_projection_compatible(
            &legacy_ebook,
            ChaptarrFormat::Ebook
        ));
        assert!(edition_projection_compatible(
            &legacy_audio,
            ChaptarrFormat::Audiobook
        ));
        assert!(edition_projection_compatible(
            &untyped,
            ChaptarrFormat::Ebook
        ));
        assert!(edition_projection_compatible(
            &untyped,
            ChaptarrFormat::Audiobook
        ));
        assert!(!edition_projection_compatible(
            &physical_claiming_ebook,
            ChaptarrFormat::Ebook
        ));

        let legacy_values = vec![json!({
            "id": 7,
            "isEbook": false,
            "language": "eng",
            "title": "The Clockwork Orchard",
            "monitored": true
        })];
        assert_eq!(
            usable_edition_count(&legacy_values, ChaptarrFormat::Audiobook),
            0
        );
        assert_eq!(
            preferred_edition_index(&legacy_values, ChaptarrFormat::Audiobook, &lookup_book()),
            None
        );
        assert!(
            sole_monitored_edition(&legacy_values, ChaptarrFormat::Audiobook).is_none(),
            "isEbook=false cannot prove that a monitored row is not physical"
        );
    }

    #[test]
    fn edition_choice_honors_the_projected_edition_and_demotes_junk() {
        let mut selected = lookup_book();
        selected.foreign_edition_id = "hc:edition-3001".into();
        let editions = vec![
            json!({"id": 1, "format": "ebook", "isEbook": true, "language": "eng", "title": "The Clockwork Orchard"}),
            json!({"id": 2, "format": "ebook", "isEbook": true, "language": "eng", "title": "The Clockwork Orchard", "foreignEditionId": "hc:edition-3001"}),
        ];
        assert_eq!(
            preferred_edition_index(&editions, ChaptarrFormat::Ebook, &selected),
            Some(1)
        );

        let junky = vec![
            json!({"id": 1, "format": "ebook", "isEbook": true, "language": "eng", "title": "Summary of The Clockwork Orchard"}),
            json!({"id": 2, "format": "ebook", "isEbook": true, "language": "eng", "title": "The Clockwork Orchard"}),
        ];
        assert_eq!(
            preferred_edition_index(&junky, ChaptarrFormat::Ebook, &selected),
            Some(1)
        );

        let projected_english_summary = vec![
            json!({
                "id": 1,
                "format": "ebook",
                "language": "eng",
                "title": "Summary of The Clockwork Orchard",
                "foreignEditionId": "hc:edition-3001"
            }),
            json!({
                "id": 2,
                "format": "ebook",
                "language": "fre",
                "title": "The Clockwork Orchard"
            }),
        ];
        assert_eq!(
            preferred_edition_index(&projected_english_summary, ChaptarrFormat::Ebook, &selected),
            Some(1),
            "the exact non-junk work must beat a projected English summary"
        );
    }

    #[test]
    fn non_english_only_works_remain_requestable() {
        let selected = selected_book("Le Comte", "hc:x");
        let editions = vec![json!({
            "id": 7,
            "format": "ebook",
            "isEbook": true,
            "language": "fre",
            "title": "Le Comte"
        })];
        assert_eq!(
            preferred_edition_index(&editions, ChaptarrFormat::Ebook, &selected),
            Some(0)
        );
    }

    #[test]
    fn english_detection_is_a_preference_marker() {
        assert!(is_english("eng"));
        assert!(is_english("en-US"));
        assert!(is_english("English"));
        assert!(!is_english("fre"));
        assert!(!is_english(""));
    }

    #[test]
    fn sole_monitored_edition_requires_exactly_one_of_the_right_format() {
        let one = json!({"id": 1, "format": "ebook", "isEbook": true, "monitored": true});
        let other = json!({"id": 2, "format": "ebook", "isEbook": true, "monitored": false});
        let found = sole_monitored_edition(&[one.clone(), other.clone()], ChaptarrFormat::Ebook)
            .expect("one monitored ebook edition");
        assert_eq!(positive_id(Some(&found.id)), Some(1));

        // Zero or two monitored editions is a failed selection.
        assert!(
            sole_monitored_edition(std::slice::from_ref(&other), ChaptarrFormat::Ebook).is_none()
        );
        let second = json!({"id": 3, "format": "ebook", "isEbook": true, "monitored": true});
        assert!(sole_monitored_edition(&[one.clone(), second], ChaptarrFormat::Ebook).is_none());
        // A monitored edition that contradicts the format is a failure too.
        assert!(sole_monitored_edition(&[one], ChaptarrFormat::Audiobook).is_none());

        let valid = json!({
            "id": 4,
            "format": "ebook",
            "monitored": true
        });
        let malformed = json!({
            "id": 5,
            "format": 42,
            "monitored": true
        });
        assert!(
            sole_monitored_edition(&[valid, malformed], ChaptarrFormat::Ebook).is_none(),
            "a malformed monitored row cannot be discarded during read-back"
        );
        assert!(
            sole_monitored_edition(
                &[json!({"id": 6, "format": "ebook", "monitored": "yes"})],
                ChaptarrFormat::Ebook
            )
            .is_none()
        );
    }

    #[test]
    fn edition_identity_fails_closed() {
        let by_id = |value: &Value| parse_edition(value).unwrap();
        assert!(same_edition(
            &by_id(&json!({"id": 5, "foreignEditionId": "hc:a"})),
            &by_id(&json!({"id": 5, "foreignEditionId": "hc:b"})),
        ));
        assert!(!same_edition(
            &by_id(&json!({"id": 5})),
            &by_id(&json!({"id": 6})),
        ));
        assert!(same_edition(
            &by_id(&json!({"foreignEditionId": "hc:a"})),
            &by_id(&json!({"foreignEditionId": "hc:a"})),
        ));
        assert!(!same_edition(&by_id(&json!({})), &by_id(&json!({}))));
    }

    #[test]
    fn catalog_commands_gate_on_author_scope_and_refreshes() {
        let scoped_running = json!([{
            "name": "RefreshAuthor",
            "status": "started",
            "body": {"authorId": 230}
        }]);
        assert!(catalog_command_active(
            scoped_running.as_array().unwrap(),
            230
        ));
        // A refresh scoped to a different author is not ours.
        assert!(!catalog_command_active(
            scoped_running.as_array().unwrap(),
            231
        ));

        let unscoped_refresh = json!([{"name": "RefreshAuthor", "status": "queued"}]);
        assert!(catalog_command_active(
            unscoped_refresh.as_array().unwrap(),
            230
        ));
        let alternate_name = json!([{"commandName": "RefreshAuthor", "status": "started"}]);
        assert!(catalog_command_active(
            alternate_name.as_array().unwrap(),
            230
        ));

        let finished = json!([{
            "name": "RefreshAuthor",
            "status": "completed",
            "body": {"authorId": 230}
        }]);
        assert!(!catalog_command_active(finished.as_array().unwrap(), 230));

        let unrelated = json!([{"name": "RssSync", "status": "started"}]);
        assert!(!catalog_command_active(unrelated.as_array().unwrap(), 230));

        let id_list = json!([{
            "name": "BulkRefresh",
            "status": "started",
            "body": {"authorIds": [12, 230]}
        }]);
        assert!(catalog_command_active(id_list.as_array().unwrap(), 230));
    }

    #[test]
    fn book_list_fingerprints_ignore_order_but_track_membership() {
        let a = json!({"id": 1, "foreignBookId": "hc:a"});
        let b = json!({"id": 2, "foreignBookId": "hc:b"});
        assert_eq!(
            book_list_fingerprint(&[a.clone(), b.clone()]),
            book_list_fingerprint(&[b.clone(), a.clone()])
        );
        assert_ne!(
            book_list_fingerprint(std::slice::from_ref(&a)),
            book_list_fingerprint(&[a, b])
        );

        let allocated = json!({
            "id": 3,
            "foreignBookId": "hc:c",
            "title": "Catalog Row",
            "mediaType": "ebook"
        });
        let populated = json!({
            "id": 3,
            "foreignBookId": "hc:c",
            "title": "Catalog Row",
            "mediaType": "ebook",
            "foreignEditionId": "hc:edition-c",
            "releaseDate": "2024-01-01",
            "images": [{"url": "https://covers.example.test/c.jpg"}]
        });
        assert_ne!(
            book_list_fingerprint(&[allocated]),
            book_list_fingerprint(&[populated]),
            "in-place metadata population must reset the settle streak"
        );
    }

    #[test]
    fn strongest_state_across_duplicate_pockets_wins() {
        let selected = selected_book("Royal Assassin", "hc:54244");
        let missing = json!({
            "title": "Royal Assassin",
            "foreignBookId": "hc:54244",
            "mediaType": "ebook",
            "monitored": false
        });
        let processing = json!({
            "title": "Royal Assassin",
            "foreignBookId": "hc:54244",
            "mediaType": "ebook",
            "monitored": true,
            "ebookMonitored": true
        });
        let other_work = json!({
            "title": "Assassin's Quest",
            "foreignBookId": "hc:99999",
            "mediaType": "ebook",
            "hasFiles": true
        });
        assert_eq!(
            format_state_across(
                &[missing.clone(), processing, other_work],
                ChaptarrFormat::Ebook,
                &selected
            ),
            FormatState::Processing
        );
        assert_eq!(
            format_state_across(&[missing], ChaptarrFormat::Ebook, &selected),
            FormatState::Missing
        );
        assert_eq!(
            format_state_across(&[], ChaptarrFormat::Ebook, &selected),
            FormatState::Missing
        );
    }

    #[test]
    fn clear_multi_book_titles_are_detected_conservatively() {
        for title in [
            "The Farseer Trilogy 3-Book Bundle",
            "The Earthsea Omnibus, Volume One",
            "Realm of the Elderlings Box Set",
            "Realm of the Elderlings Boxed Sets",
            "The Complete Broken Earth Trilogy",
            "The Complete Discworld Series",
            "Three-Book Collection",
            "A Collection of 4 Books",
        ] {
            assert!(
                clear_multi_book_result_title(title),
                "expected a clear multi-book result: {title}"
            );
        }

        for title in [
            "Stories: A Collection",
            "The Lottery and Other Stories",
            "Bundle of Joy",
            "Trilogy of Terror",
            "A Series of Unfortunate Events",
            "Fourier Series",
            "A Collection of 4 Stories",
            "The Book of Three",
            "Collected Poems",
        ] {
            assert!(
                !clear_multi_book_result_title(title),
                "must not reject an ordinary single-work title: {title}"
            );
        }
    }
}
