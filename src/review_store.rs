//! Review decisions and removal marks saved to a file, for the review server.
//!
//! The file uses the same `{ schema_version: 1, decisions: [...] }` shape as
//! the report's "Export decisions" backup, so an exported file can be dropped
//! in place and a saved file can be imported into a static report. Removal
//! marks live beside the decisions under `removed_pins`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

const SCHEMA_VERSION: u64 = 1;
const STORE_FILE: &str = "review.json";
const MAX_DECISIONS: usize = 100_000;
const DECISION_ACTIONS: [&str; 5] = ["reviewed", "keep_all", "remove", "later", "not_duplicate"];

#[derive(Debug, Error)]
pub enum ReviewStoreError {
    #[error("review file {path} could not be read or written")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("review file {path} is not valid; it was left untouched")]
    Invalid { path: PathBuf },
    #[error("invalid review decision: {0}")]
    InvalidDecision(&'static str),
    #[error("no folder is available for review data; set UNPIN_DATA_DIR")]
    NoDataDir,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReviewState {
    pub schema_version: u64,
    pub decisions: Vec<Value>,
    /// Pin ID to the Unix time, in seconds, when the pin was found removed.
    #[serde(default)]
    pub removed_pins: BTreeMap<String, u64>,
}

/// A change sent by the report page: decisions to set or replace by key, and
/// keys whose decisions were cleared.
#[derive(Debug, Default, Deserialize)]
pub struct DecisionChanges {
    #[serde(default)]
    pub set: Vec<Value>,
    #[serde(default)]
    pub remove: Vec<String>,
}

pub struct ReviewStore {
    path: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl ReviewStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The default store: `review.json` in `UNPIN_DATA_DIR`, or in the
    /// operating system's per-user data folder.
    pub fn open_default() -> Result<Self, ReviewStoreError> {
        default_data_dir()
            .map(|dir| Self::new(dir.join(STORE_FILE)))
            .ok_or(ReviewStoreError::NoDataDir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load(&self) -> Result<ReviewState, ReviewStoreError> {
        let _guard = self.lock.lock().await;
        self.read()
    }

    /// Merges page changes into the file. The file is read again under the
    /// lock first, so two reports open at once keep each other's decisions.
    pub async fn apply(&self, changes: DecisionChanges) -> Result<(), ReviewStoreError> {
        for decision in &changes.set {
            validate_decision(decision)?;
        }
        if changes.remove.iter().any(|key| !valid_key(key)) {
            return Err(ReviewStoreError::InvalidDecision("key"));
        }
        let _guard = self.lock.lock().await;
        let mut state = self.read()?;
        let mut by_key: BTreeMap<String, Value> = BTreeMap::new();
        let mut order = Vec::new();
        for decision in state.decisions.drain(..) {
            if let Some(key) = decision.get("key").and_then(Value::as_str) {
                if !by_key.contains_key(key) {
                    order.push(key.to_owned());
                }
                by_key.insert(key.to_owned(), decision);
            }
        }
        for key in &changes.remove {
            by_key.remove(key);
        }
        for decision in changes.set {
            let key = decision["key"].as_str().unwrap_or_default().to_owned();
            if !by_key.contains_key(&key) {
                order.push(key.clone());
            }
            by_key.insert(key, decision);
        }
        state.decisions = order
            .into_iter()
            .filter_map(|key| by_key.remove(&key))
            .collect();
        if state.decisions.len() > MAX_DECISIONS {
            return Err(ReviewStoreError::InvalidDecision("too many decisions"));
        }
        self.write(&state)
    }

    pub async fn set_removed(&self, pin_id: &str, removed: bool) -> Result<(), ReviewStoreError> {
        let _guard = self.lock.lock().await;
        let mut state = self.read()?;
        let changed = if removed {
            if state.removed_pins.contains_key(pin_id) {
                false
            } else {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or_default();
                state.removed_pins.insert(pin_id.to_owned(), now);
                true
            }
        } else {
            state.removed_pins.remove(pin_id).is_some()
        };
        if changed { self.write(&state) } else { Ok(()) }
    }

    fn read(&self) -> Result<ReviewState, ReviewStoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReviewState {
                    schema_version: SCHEMA_VERSION,
                    ..ReviewState::default()
                });
            }
            Err(source) => {
                return Err(ReviewStoreError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let state: ReviewState =
            serde_json::from_slice(&bytes).map_err(|_| ReviewStoreError::Invalid {
                path: self.path.clone(),
            })?;
        if state.schema_version != SCHEMA_VERSION {
            return Err(ReviewStoreError::Invalid {
                path: self.path.clone(),
            });
        }
        Ok(state)
    }

    /// Writes a complete file next to the old one, then swaps it in, so a
    /// crash never leaves half a file behind.
    fn write(&self, state: &ReviewState) -> Result<(), ReviewStoreError> {
        let io_error = |source| ReviewStoreError::Io {
            path: self.path.clone(),
            source,
        };
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(io_error)?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| io_error(error.into()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        temporary.write_all(&bytes).map_err(io_error)?;
        temporary.as_file().sync_all().map_err(io_error)?;
        temporary
            .persist(&self.path)
            .map_err(|error| io_error(error.error))?;
        Ok(())
    }
}

/// Mirrors the report page's own checks closely enough that the file never
/// holds an entry the page would reject on load.
fn validate_decision(value: &Value) -> Result<(), ReviewStoreError> {
    let invalid = ReviewStoreError::InvalidDecision;
    let object: &Map<String, Value> = value.as_object().ok_or(invalid("not an object"))?;
    let key = object
        .get("key")
        .and_then(Value::as_str)
        .ok_or(invalid("key"))?;
    if !valid_key(key) {
        return Err(invalid("key"));
    }
    let action = object
        .get("action")
        .and_then(Value::as_str)
        .ok_or(invalid("action"))?;
    if !DECISION_ACTIONS.contains(&action) {
        return Err(invalid("action"));
    }
    let reviewed = object
        .get("reviewed")
        .and_then(Value::as_bool)
        .ok_or(invalid("reviewed"))?;
    if (action == "later" && reviewed) || (action != "later" && action != "remove" && !reviewed) {
        return Err(invalid("reviewed"));
    }
    let items = object
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or(invalid("items"))?;
    let mut pin_ids = std::collections::HashSet::new();
    for item in items {
        let field = |name: &str| item.get(name).and_then(Value::as_str);
        let pin_id = field("pin_id")
            .filter(|id| !id.is_empty())
            .ok_or(invalid("items"))?;
        if ["pin_url", "image_url", "image_sha256", "media_kind"]
            .iter()
            .any(|name| field(name).is_none())
            || !pin_ids.insert(pin_id)
        {
            return Err(invalid("items"));
        }
    }
    let remove = object
        .get("remove_pin_ids")
        .and_then(Value::as_array)
        .ok_or(invalid("remove_pin_ids"))?;
    let mut remove_ids = std::collections::HashSet::new();
    for id in remove {
        let id = id.as_str().ok_or(invalid("remove_pin_ids"))?;
        if !pin_ids.contains(id) || !remove_ids.insert(id) {
            return Err(invalid("remove_pin_ids"));
        }
    }
    if (action == "remove") == remove.is_empty() {
        return Err(invalid("remove_pin_ids"));
    }
    Ok(())
}

/// `exact|visual` : `same-board|cross-board` : 64 lowercase hex digits.
fn valid_key(key: &str) -> bool {
    let mut parts = key.splitn(3, ':');
    matches!(parts.next(), Some("exact" | "visual"))
        && matches!(parts.next(), Some("same-board" | "cross-board"))
        && parts.next().is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn default_data_dir() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("UNPIN_DATA_DIR").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(configured));
    }
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };

    #[cfg(target_os = "macos")]
    {
        absolute("HOME").map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("unpin")
        })
    }

    #[cfg(target_os = "windows")]
    {
        absolute("APPDATA").map(|data| data.join("unpin"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".local").join("share")))
            .map(|data| data.join("unpin"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision(key: &str, action: &str, reviewed: bool, remove: &[&str]) -> Value {
        json!({
            "key": key, "action": action, "reviewed": reviewed, "remove_pin_ids": remove,
            "items": [
                {"pin_id": "1", "pin_url": "https://www.pinterest.com/pin/1/", "image_url": "https://i.pinimg.com/1.jpg", "image_sha256": "a", "media_kind": "image"},
                {"pin_id": "2", "pin_url": "https://www.pinterest.com/pin/2/", "image_url": "https://i.pinimg.com/2.jpg", "image_sha256": "a", "media_kind": "image"}
            ]
        })
    }

    fn key(n: char) -> String {
        format!("exact:same-board:{}", n.to_string().repeat(64))
    }

    #[tokio::test]
    async fn decisions_merge_by_key_and_survive_reload() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(directory.path().join("nested").join("review.json"));
        assert!(store.load().await.unwrap().decisions.is_empty());

        store
            .apply(DecisionChanges {
                set: vec![
                    decision(&key('a'), "reviewed", true, &[]),
                    decision(&key('b'), "remove", false, &["2"]),
                ],
                remove: Vec::new(),
            })
            .await
            .unwrap();
        store
            .apply(DecisionChanges {
                set: vec![decision(&key('b'), "remove", true, &["2"])],
                remove: vec![key('a')],
            })
            .await
            .unwrap();

        let reopened = ReviewStore::new(store.path().to_path_buf());
        let state = reopened.load().await.unwrap();
        assert_eq!(
            state.decisions,
            vec![decision(&key('b'), "remove", true, &["2"])]
        );
    }

    #[tokio::test]
    async fn invalid_decisions_are_rejected_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(directory.path().join("review.json"));
        for bad in [
            decision("exact:same-board:short", "reviewed", true, &[]),
            decision(&key('a'), "delete", true, &[]),
            decision(&key('a'), "remove", true, &[]),
            decision(&key('a'), "remove", true, &["9"]),
            decision(&key('a'), "keep_all", false, &[]),
            json!("not an object"),
        ] {
            let result = store
                .apply(DecisionChanges {
                    set: vec![bad],
                    remove: Vec::new(),
                })
                .await;
            assert!(matches!(result, Err(ReviewStoreError::InvalidDecision(_))));
        }
        assert!(!store.path().exists());
    }

    #[tokio::test]
    async fn removal_marks_are_recorded_and_cleared() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(directory.path().join("review.json"));
        store.set_removed("42", true).await.unwrap();
        let first = store.load().await.unwrap().removed_pins["42"];
        store.set_removed("42", true).await.unwrap();
        assert_eq!(store.load().await.unwrap().removed_pins["42"], first);
        store.set_removed("42", false).await.unwrap();
        assert!(store.load().await.unwrap().removed_pins.is_empty());
    }

    #[tokio::test]
    async fn a_corrupt_file_is_reported_and_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("review.json");
        std::fs::write(&path, b"{not json").unwrap();
        let store = ReviewStore::new(path.clone());
        assert!(matches!(
            store.load().await,
            Err(ReviewStoreError::Invalid { .. })
        ));
        assert!(store.set_removed("1", true).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    #[test]
    fn exported_decision_files_load_as_stores() {
        let exported =
            json!({"schema_version": 1, "decisions": [decision(&key('c'), "keep_all", true, &[])]});
        let state: ReviewState = serde_json::from_value(exported).unwrap();
        assert!(state.removed_pins.is_empty());
        assert_eq!(state.decisions.len(), 1);
    }
}
