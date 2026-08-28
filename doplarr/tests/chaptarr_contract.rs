//! Contract tests for the narrow Chaptarr surface. Two drift alarms:
//!
//! 1. Route inventory: every route the client depends on must exist in the
//!    vendored `openapi_paths.json` extract, refreshed from a Chaptarr clone
//!    with `.github/ci/refresh-openapi-extract.sh`. Route inventory ONLY -
//!    the spec mistypes command bodies and parameter optionality and omits
//!    controller 400s, so no schema may ever be generated from it.
//! 2. Serializer traps: every checked-in Chaptarr fixture must obey the
//!    0.9.936 serializer (`STJson.cs:27` omits nulls; `RestResource.cs:7-8`
//!    omits `id: 0`; `grabbed` never reaches REST rows,
//!    `BookResource.cs:87-88,236`; `/book` responses never carry an
//!    `editions` key, `BookResource.cs:137-259`; metadata `profileType` is
//!    numeric while quality `profileType` is a camelCase string).

use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/chaptarr");

/// The 14 routes the client's contract stands on. This list is OURS: it
/// changes only when the client's endpoint usage changes, never to chase a
/// server release. The last two are not called by the request pipeline but
/// are documented landmarks (the author-monitor landmine and the
/// wanted-editions endpoint under decision record 0001) whose disappearance
/// would signal a contract-relevant server change.
const DEPENDED_ON_ROUTES: [&str; 14] = [
    "/api/v1/system/status",
    "/api/v1/book/lookup",
    "/api/v1/author",
    "/api/v1/author/{id}",
    "/api/v1/book",
    "/api/v1/book/{id}",
    "/api/v1/book/monitor",
    "/api/v1/edition",
    "/api/v1/command",
    "/api/v1/qualityprofile",
    "/api/v1/metadataprofile",
    "/api/v1/rootfolder",
    "/api/v1/author/{id}/monitor/{mediaType}",
    "/api/v1/book/{id}/editions/wanted",
];

/// Not Chaptarr serializer output: the Open Library response fixture and our
/// own vendored route extract.
const NON_CHAPTARR_FIXTURES: [&str; 2] = ["openlibrary_search.json", "openapi_paths.json"];

/// Fixtures whose top level is a `/book` BookResource (row, list, or POST
/// echo) and therefore must not carry an `editions` key anywhere.
const BOOK_ROW_FIXTURES: [&str; 5] = [
    "book_available.json",
    "book_processing.json",
    "book_sparse.json",
    "book_unmonitored.json",
    "post_book_response.json",
];

fn missing_routes(paths: &BTreeSet<String>) -> Vec<&'static str> {
    DEPENDED_ON_ROUTES
        .iter()
        .copied()
        .filter(|route| !paths.contains(*route))
        .collect()
}

fn serializer_violations(fixture: &str, value: &Value, path: &str, violations: &mut Vec<String>) {
    match value {
        Value::Null => violations.push(format!(
            "{fixture}: explicit null at {path} (the serializer omits null properties)"
        )),
        Value::Object(fields) => {
            for (key, child) in fields {
                if key == "grabbed" {
                    violations.push(format!(
                        "{fixture}: `grabbed` at {path} (SignalR-only; never on REST rows)"
                    ));
                }
                if key == "id" && child == &json!(0) {
                    violations.push(format!(
                        "{fixture}: `id: 0` at {path} (the serializer omits default ids)"
                    ));
                }
                serializer_violations(fixture, child, &format!("{path}.{key}"), violations);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                serializer_violations(fixture, item, &format!("{path}[{index}]"), violations);
            }
        }
        _ => {}
    }
}

fn editions_key_violations(fixture: &str, value: &Value, violations: &mut Vec<String>) {
    let rows: Vec<&Value> = match value {
        Value::Array(rows) => rows.iter().collect(),
        row => vec![row],
    };
    for row in rows {
        if row.get("editions").is_some() {
            violations.push(format!(
                "{fixture}: `editions` key on a /book row (BookResource.cs:137-259 never assigns it; /edition is the only edition source)"
            ));
        }
    }
}

fn profile_type_violations(
    fixture: &str,
    value: &Value,
    numeric: bool,
    violations: &mut Vec<String>,
) {
    for profile in value.as_array().into_iter().flatten() {
        let profile_type = profile.get("profileType");
        let ok = if numeric {
            profile_type.is_some_and(Value::is_number)
        } else {
            profile_type.is_some_and(Value::is_string)
        };
        if !ok {
            violations.push(format!(
                "{fixture}: profileType must be {} (metadata profiles serialize the enum as int, quality profiles as camelCase string)",
                if numeric { "numeric" } else { "a string" }
            ));
        }
    }
}

fn fixture_json(name: &str) -> Value {
    let raw = fs::read_to_string(Path::new(FIXTURES).join(name))
        .unwrap_or_else(|error| panic!("could not read fixture {name}: {error}"));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("invalid JSON in {name}: {error}"))
}

/// The route extract to check: the vendored fixture, or whatever
/// `CHAPTARR_OPENAPI_PATHS` points at. That override exists for the
/// release-watch script (`scripts/check-chaptarr-release.sh`), which runs this
/// one test against an extract taken fresh from a candidate Chaptarr tag. Only
/// this test honors it.
fn openapi_paths_extract() -> (String, Value) {
    let path = match env::var("CHAPTARR_OPENAPI_PATHS") {
        Ok(overridden) => PathBuf::from(overridden),
        Err(_) => Path::new(FIXTURES).join("openapi_paths.json"),
    };
    let label = path.display().to_string();
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read route extract {label}: {error}"));
    let value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("invalid JSON in {label}: {error}"));
    (label, value)
}

#[test]
fn depended_on_routes_exist_in_the_vendored_openapi_extract() {
    let (extract, value) = openapi_paths_extract();
    let paths: BTreeSet<String> = serde_json::from_value::<Vec<String>>(value)
        .unwrap_or_else(|error| panic!("{extract} must be an array of route strings: {error}"))
        .into_iter()
        .collect();
    assert!(
        paths.len() > 100,
        "the route extract {extract} looks truncated ({} paths); refresh it with .github/ci/refresh-openapi-extract.sh",
        paths.len()
    );
    let missing = missing_routes(&paths);
    assert!(
        missing.is_empty(),
        "depended-on routes missing from the openapi extract {extract}: {missing:?}. If a Chaptarr release removed them, the client contract must be revisited - do not just refresh the extract."
    );
}

#[test]
fn a_dropped_route_is_detected() {
    let mut paths: BTreeSet<String> = DEPENDED_ON_ROUTES.iter().map(|s| s.to_string()).collect();
    paths.remove("/api/v1/book/monitor");
    assert_eq!(missing_routes(&paths), vec!["/api/v1/book/monitor"]);
}

#[test]
fn every_chaptarr_fixture_obeys_the_serializer_traps() {
    let mut violations = Vec::new();
    let mut seen = 0;
    for entry in fs::read_dir(FIXTURES).expect("fixture directory must exist") {
        let path = entry.unwrap().path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if !name.ends_with(".json") || NON_CHAPTARR_FIXTURES.contains(&name.as_str()) {
            continue;
        }
        seen += 1;
        let value = fixture_json(&name);
        serializer_violations(&name, &value, "$", &mut violations);
        if BOOK_ROW_FIXTURES.contains(&name.as_str()) {
            editions_key_violations(&name, &value, &mut violations);
        }
        if name == "metadata_profiles.json" {
            profile_type_violations(&name, &value, true, &mut violations);
        }
        if name == "quality_profiles.json" {
            profile_type_violations(&name, &value, false, &mut violations);
        }
    }
    assert!(
        seen >= 10,
        "expected the Chaptarr fixture set, found {seen} files"
    );
    assert!(
        violations.is_empty(),
        "fixture serializer-trap violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_sweep_rejects_nulls_zero_ids_grabbed_and_book_row_editions() {
    let mut violations = Vec::new();
    serializer_violations(
        "inline",
        &json!({
            "id": 0,
            "overview": null,
            "grabbed": false,
            "rows": [{"id": 5, "remoteCover": null}]
        }),
        "$",
        &mut violations,
    );
    assert_eq!(
        violations.len(),
        4,
        "unexpected sweep report: {violations:?}"
    );
    assert!(violations.iter().any(|v| v.contains("`id: 0` at $")));
    assert!(
        violations
            .iter()
            .any(|v| v.contains("explicit null at $.overview"))
    );
    assert!(violations.iter().any(|v| v.contains("`grabbed`")));
    assert!(
        violations
            .iter()
            .any(|v| v.contains("$.rows[0].remoteCover"))
    );

    let mut editions = Vec::new();
    editions_key_violations("inline", &json!([{"id": 7, "editions": []}]), &mut editions);
    assert_eq!(editions.len(), 1);

    let mut profile_types = Vec::new();
    profile_type_violations(
        "inline",
        &json!([{"id": 1, "profileType": "ebook"}]),
        true,
        &mut profile_types,
    );
    profile_type_violations(
        "inline",
        &json!([{"id": 1, "profileType": 2}]),
        false,
        &mut profile_types,
    );
    assert_eq!(profile_types.len(), 2);
}
