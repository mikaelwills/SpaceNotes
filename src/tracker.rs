use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ContentTracker {
    // Maps UUID -> "a1b2c3..." (SHA256 hash)
    hashes: Arc<Mutex<HashMap<String, String>>>,
}

impl ContentTracker {
    pub fn new() -> Self {
        Self {
            hashes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Calculate hash of content string
    pub fn hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Update the tracker with new content (e.g., after downloading from Server)
    pub fn update(&self, id: &str, content: &str) {
        let hash = Self::hash(content);
        let mut map = self.hashes.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(id.to_string(), hash);
    }

    /// Check if content has changed WITHOUT updating the tracker (read-only)
    pub fn has_changed(&self, id: &str, current_content: &str) -> bool {
        let new_hash = Self::hash(current_content);
        let map = self.hashes.lock().unwrap_or_else(|e| e.into_inner());

        match map.get(id) {
            Some(old_hash) => *old_hash != new_hash,
            None => true, // New file or not tracked yet
        }
    }

    /// Check if the content on disk is different from what we last synced/downloaded
    /// Updates the tracker with the new hash as a side effect
    pub fn is_modified(&self, id: &str, current_content: &str) -> bool {
        let new_hash = Self::hash(current_content);
        let mut map = self.hashes.lock().unwrap_or_else(|e| e.into_inner());

        match map.get(id) {
            Some(old_hash) if *old_hash == new_hash => {
                // Content matches what we tracked -> No change
                false
            }
            _ => {
                // Content differs (or new file) -> Update tracker and return true
                map.insert(id.to_string(), new_hash);
                true
            }
        }
    }

    /// Remove an ID from the tracker (e.g., when file is deleted)
    pub fn remove(&self, id: &str) {
        let mut map = self.hashes.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poison_tracker(tracker: &ContentTracker) {
        let hashes = tracker.hashes.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = hashes.lock().unwrap_or_else(|e| e.into_inner());
            panic!("panic while holding tracker lock");
        }));
        assert!(result.is_err(), "expected the guarded closure to panic");
    }

    #[test]
    fn cascade_poison_recovers_on_subsequent_ops() {
        let tracker = ContentTracker::new();
        tracker.update("id-a", "content-a");

        poison_tracker(&tracker);

        tracker.update("id-b", "content-b");
        assert!(tracker.has_changed("id-c", "new"));
        assert!(!tracker.is_modified("id-b", "content-b"));
        tracker.remove("id-b");
    }
}
