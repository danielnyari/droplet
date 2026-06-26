//! The generic handle registry — Droplet's boundary seam (invariant #6).
//!
//! Engine objects (a DuckDB connection, a materialized result) live host-side
//! inside the registry; the sandbox only ever receives an opaque `u64` handle.

use std::collections::HashMap;

use crate::DropletError;

/// A host-side store of values keyed by a monotonic `u64` handle.
///
/// `T` is generic: it holds a `String` in tests today, the real engine-handle
/// type when DuckDB lands in M1.
pub struct Registry<T> {
    next: u64,
    items: HashMap<u64, T>,
}

impl<T> Registry<T> {
    pub fn new() -> Self {
        Self {
            next: 0,
            items: HashMap::new(),
        }
    }

    /// Store a value and return its fresh handle.
    pub fn insert(&mut self, value: T) -> u64 {
        let id = self.next;
        self.next += 1; // monotonic: never hand out the same id twice
        self.items.insert(id, value);
        id
    }

    /// Borrow the value behind a handle, or `None` if there is none.
    pub fn get(&self, handle: u64) -> Option<&T> {
        self.items.get(&handle)
    }

    /// Remove and return the owned value behind a handle (cleanup path).
    pub fn remove(&mut self, handle: u64) -> Option<T> {
        self.items.remove(&handle)
    }

    /// Borrow the value behind a handle, turning a miss into a `DropletError`.
    /// This is the exact move engine functions make when the sandbox passes a
    /// bad handle: `reg.require(h)?`.
    pub fn require(&self, handle: u64) -> Result<&T, DropletError> {
        self.get(handle).ok_or(DropletError::BadHandle(handle))
    }
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get_roundtrips() {
        let mut reg: Registry<String> = Registry::new();
        let h = reg.insert("hello".to_string());
        assert_eq!(reg.get(h), Some(&"hello".to_string()));
    }

    #[test]
    fn handles_are_unique_and_missing_is_none() {
        let mut reg: Registry<u32> = Registry::new();
        let a = reg.insert(1);
        let b = reg.insert(2);
        assert_ne!(a, b); // monotonic counter never repeats
        assert_eq!(reg.get(999), None); // never-issued handle
    }

    #[test]
    fn remove_then_get_is_none() {
        let mut reg: Registry<u32> = Registry::new();
        let h = reg.insert(42);
        assert_eq!(reg.remove(h), Some(42));
        assert_eq!(reg.get(h), None);
    }

    #[test]
    fn require_missing_is_bad_handle() {
        let reg: Registry<u32> = Registry::new();
        assert!(matches!(
            reg.require(999),
            Err(DropletError::BadHandle(999))
        ));
    }
}
