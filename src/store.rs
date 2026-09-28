//! What lshn keeps between runs, in `~/.lshn/`: which stories you've read
//! and how much of their threads you'd seen (`seen.json`), and a cache of
//! what it fetched (`cache/`), so the last lists, stories and threads show
//! the moment it starts, and articles aren't fetched twice.
//!
//! Losing any of it only costs a fetch, so errors reading or writing it are
//! ignored.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Cached things older than this are deleted at startup.
const KEEP_CACHE: Duration = Duration::from_secs(7 * 24 * 3600);
/// Stories read longer ago than this are forgotten.
const KEEP_SEEN: u64 = 90 * 24 * 3600;

/// `~/.lshn`.
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".lshn"))
}

/// What you'd seen of a story when you last read it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// When, in seconds since the epoch.
    pub at: u64,
    /// The newest comment then. HN's ids only grow, so any comment with a
    /// bigger one is new.
    pub newest: u64,
    /// How many comments HN counted then, for the list's "+N" before the
    /// thread's loaded.
    pub count: u64,
}

/// Which stories have been read, by id.
pub struct SeenStore {
    file: Option<PathBuf>,
    pub stories: HashMap<u64, Seen>,
}

impl SeenStore {
    pub fn load(dir: Option<&Path>, now: u64) -> SeenStore {
        let file = dir.map(|d| d.join("seen.json"));
        let mut stories: HashMap<u64, Seen> = file
            .as_deref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        stories.retain(|_, s| now.saturating_sub(s.at) < KEEP_SEEN);
        SeenStore { file, stories }
    }

    pub fn save(&self) {
        if let Some(file) = &self.file
            && let Ok(json) = serde_json::to_vec(&self.stories)
        {
            write(file, &json);
        }
    }
}

/// Where fetched things are kept: `cache/<kind>/<key>.json`.
#[derive(Clone)]
pub struct Cache {
    dir: Option<PathBuf>,
}

impl Cache {
    pub fn new(dir: Option<&Path>) -> Cache {
        Cache {
            dir: dir.map(|d| d.join("cache")),
        }
    }

    /// No cache: for tests, and when there's no home directory.
    #[cfg(test)]
    pub fn none() -> Cache {
        Cache { dir: None }
    }

    fn path(&self, kind: &str, key: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(kind).join(format!("{key}.json")))
    }

    pub fn get<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Option<T> {
        let bytes = std::fs::read(self.path(kind, key)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn put<T: Serialize>(&self, kind: &str, key: &str, value: &T) {
        if let (Some(path), Ok(json)) = (self.path(kind, key), serde_json::to_vec(value)) {
            write(&path, &json);
        }
    }

    /// Kept as they came, rather than as JSON: pictures.
    pub fn get_bytes(&self, kind: &str, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.dir.as_ref()?.join(kind).join(key)).ok()
    }

    pub fn put_bytes(&self, kind: &str, key: &str, bytes: &[u8]) {
        if let Some(dir) = &self.dir {
            write(&dir.join(kind).join(key), bytes);
        }
    }

    /// Deletes what's older than a week, in the background.
    pub fn prune(&self) {
        let Some(dir) = self.dir.clone() else { return };
        std::thread::spawn(move || {
            let now = SystemTime::now();
            let Ok(kinds) = std::fs::read_dir(&dir) else {
                return;
            };
            for kind in kinds.flatten() {
                let Ok(files) = std::fs::read_dir(kind.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let old = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age > KEEP_CACHE);
                    if old {
                        let _ = std::fs::remove_file(file.path());
                    }
                }
            }
        });
    }
}

/// Writes `bytes` beside `path` and renames it into place, so a reader
/// never sees half a file.
fn write(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Unique per thread, since workers write at the same time.
    let tmp = path.with_extension(format!("tmp{:?}", std::thread::current().id()).replace(['(', ')'], ""));
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lshn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn remembers_what_was_seen_and_forgets_it_in_time() {
        let dir = temp("seen");
        let mut seen = SeenStore::load(Some(&dir), 0);
        assert!(seen.stories.is_empty());
        let s = Seen {
            at: 1000,
            newest: 42,
            count: 7,
        };
        seen.stories.insert(1, s);
        seen.stories.insert(2, Seen { at: 0, ..s });
        seen.save();
        let later = SeenStore::load(Some(&dir), 1000 + KEEP_SEEN - 1);
        assert_eq!(later.stories.get(&1), Some(&s));
        assert_eq!(later.stories.get(&2), None, "read too long ago");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn caches_by_kind_and_key() {
        let dir = temp("cache");
        let cache = Cache::new(Some(&dir));
        assert_eq!(cache.get::<Vec<u64>>("feed", "top"), None);
        cache.put("feed", "top", &vec![3u64, 1, 2]);
        assert_eq!(cache.get::<Vec<u64>>("feed", "top"), Some(vec![3, 1, 2]));
        assert!(dir.join("cache/feed/top.json").is_file());
        // Nothing left over from writing it.
        assert_eq!(std::fs::read_dir(dir.join("cache/feed")).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
