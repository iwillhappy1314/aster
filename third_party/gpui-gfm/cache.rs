//! LRU cache for parsed markdown documents.
//!
//! Avoids re-parsing identical markdown source strings on every render.
//! The cache is instance-based (not global static) so consumers control
//! lifetime and can share it across multiple render sites.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::parse;
use crate::types::ParsedMarkdown;

/// Default maximum number of cached entries.
pub const DEFAULT_MAX_ENTRIES: usize = 256;

/// Sources longer than this are never cached (parsing is cheap relative
/// to the memory cost of keeping them around).
pub const DEFAULT_MAX_SOURCE_LEN: usize = 100_000;

/// LRU cache mapping source strings → parsed markdown IR.
#[derive(Debug)]
pub struct MarkdownCache {
  entries: HashMap<Arc<str>, ParsedMarkdown>,
  lru_keys: VecDeque<Arc<str>>,
  max_entries: usize,
  max_source_len: usize,
}

impl Default for MarkdownCache {
  fn default() -> Self {
    Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_SOURCE_LEN)
  }
}

impl MarkdownCache {
  /// Create a cache with custom limits.
  pub fn new(max_entries: usize, max_source_len: usize) -> Self {
    Self {
      entries: HashMap::with_capacity(max_entries.min(64)),
      lru_keys: VecDeque::with_capacity(max_entries.min(64)),
      max_entries: max_entries.max(1),
      max_source_len,
    }
  }

  /// Look up a previously parsed document. Returns `None` on cache miss.
  /// On hit the entry is promoted to most-recently-used.
  pub fn get(&mut self, source: &str) -> Option<ParsedMarkdown> {
    if !self.entries.contains_key(source) {
      return None;
    }
    self.touch(source);
    self.entries.get(source).cloned()
  }

  /// Parse `source` (or return a cached result) and return the IR.
  ///
  /// Sources exceeding `max_source_len` are parsed but not cached.
  pub fn get_or_parse(&mut self, source: &str) -> ParsedMarkdown {
    if let Some(cached) = self.get(source) {
      return cached;
    }

    let parsed = parse::parse_markdown(source);

    if source.len() <= self.max_source_len {
      self.insert(Arc::from(source), parsed.clone());
    }

    parsed
  }

  /// Insert a pre-parsed entry. If the key already exists its LRU
  /// position is refreshed but the value is **not** replaced.
  pub fn insert(&mut self, source: Arc<str>, parsed: ParsedMarkdown) {
    if self.entries.contains_key(source.as_ref()) {
      self.touch(source.as_ref());
      return;
    }

    self.entries.insert(source.clone(), parsed);
    self.lru_keys.push_back(source);
    self.evict_excess();
  }

  /// Remove all entries.
  pub fn clear(&mut self) {
    self.entries.clear();
    self.lru_keys.clear();
  }

  /// Number of entries currently cached.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// Whether the cache is empty.
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// Maximum number of entries this cache will hold.
  pub fn max_entries(&self) -> usize {
    self.max_entries
  }

  // -- internals --

  fn touch(&mut self, source: &str) {
    if let Some(ix) = self.lru_keys.iter().position(|key| key.as_ref() == source) {
      if let Some(key) = self.lru_keys.remove(ix) {
        self.lru_keys.push_back(key);
      }
    }
  }

  fn evict_excess(&mut self) {
    while self.entries.len() > self.max_entries {
      let Some(oldest) = self.lru_keys.pop_front() else {
        break;
      };
      self.entries.remove(oldest.as_ref());
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cache_miss_returns_none() {
    let mut cache = MarkdownCache::default();
    assert!(cache.get("**hello**").is_none());
  }

  #[test]
  fn cache_hit_returns_same_arc() {
    let mut cache = MarkdownCache::default();
    let source = "**hello**";
    let parsed = parse::parse_markdown(source);
    let ptr = Arc::as_ptr(&parsed.blocks);

    cache.insert(Arc::from(source), parsed);
    let cached = cache.get(source).expect("should hit");
    assert_eq!(Arc::as_ptr(&cached.blocks), ptr);
  }

  #[test]
  fn get_or_parse_caches_on_first_call() {
    let mut cache = MarkdownCache::default();
    let source = "# Title";

    let first = cache.get_or_parse(source);
    let second = cache.get_or_parse(source);

    assert_eq!(Arc::as_ptr(&first.blocks), Arc::as_ptr(&second.blocks));
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn get_or_parse_skips_cache_for_large_source() {
    let mut cache = MarkdownCache::new(256, 10);
    let source = "a".repeat(20); // 20 chars > max_source_len=10

    let first = cache.get_or_parse(&source);
    let second = cache.get_or_parse(&source);

    // Both parse successfully but are NOT the same Arc (not cached).
    assert_ne!(Arc::as_ptr(&first.blocks), Arc::as_ptr(&second.blocks));
    assert_eq!(cache.len(), 0);
  }

  #[test]
  fn evicts_oldest_when_full() {
    let mut cache = MarkdownCache::new(3, DEFAULT_MAX_SOURCE_LEN);

    cache.get_or_parse("a");
    cache.get_or_parse("b");
    cache.get_or_parse("c");
    assert_eq!(cache.len(), 3);

    cache.get_or_parse("d");
    assert_eq!(cache.len(), 3);
    assert!(cache.get("a").is_none());
    assert!(cache.get("d").is_some());
  }

  #[test]
  fn get_refreshes_lru_order() {
    let mut cache = MarkdownCache::new(3, DEFAULT_MAX_SOURCE_LEN);

    cache.get_or_parse("a");
    cache.get_or_parse("b");
    cache.get_or_parse("c");

    // Touch "a" — now "b" is the oldest.
    cache.get("a");

    cache.get_or_parse("d");
    assert_eq!(cache.len(), 3);
    assert!(cache.get("a").is_some()); // refreshed
    assert!(cache.get("b").is_none()); // evicted
  }

  #[test]
  fn insert_existing_key_refreshes_but_does_not_replace() {
    let mut cache = MarkdownCache::new(3, DEFAULT_MAX_SOURCE_LEN);

    let first = parse::parse_markdown("x");
    let first_ptr = Arc::as_ptr(&first.blocks);
    cache.insert(Arc::from("x"), first);

    let second = parse::parse_markdown("x");
    let second_ptr = Arc::as_ptr(&second.blocks);
    cache.insert(Arc::from("x"), second);

    let cached = cache.get("x").unwrap();
    assert_eq!(Arc::as_ptr(&cached.blocks), first_ptr);
    assert_ne!(Arc::as_ptr(&cached.blocks), second_ptr);
  }

  #[test]
  fn clear_empties_cache() {
    let mut cache = MarkdownCache::default();
    cache.get_or_parse("a");
    cache.get_or_parse("b");
    assert_eq!(cache.len(), 2);

    cache.clear();
    assert!(cache.is_empty());
    assert!(cache.get("a").is_none());
  }

  #[test]
  fn max_entries_is_at_least_one() {
    let cache = MarkdownCache::new(0, DEFAULT_MAX_SOURCE_LEN);
    assert_eq!(cache.max_entries(), 1);
  }
}
