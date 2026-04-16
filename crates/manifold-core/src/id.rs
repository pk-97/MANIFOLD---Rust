// Typed identifiers for compile-time safety.
//
// Prevents mixing clip IDs, layer IDs, and effect group IDs at the type level.
// All wrap `Arc<str>` for cheap cloning (atomic ref-count bump instead of heap
// allocation) with serde(transparent) for backwards-compatible JSON.

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

/// Clip identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipId(Arc<str>);

/// Layer identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LayerId(Arc<str>);

/// Effect group identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectGroupId(Arc<str>);

/// Effect instance identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectId(Arc<str>);

/// Timeline marker identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MarkerId(Arc<str>);

// ── Macro for shared impls ──

macro_rules! impl_id_type {
    ($T:ident) => {
        impl $T {
            pub fn new(s: impl AsRef<str>) -> Self {
                Self(Arc::from(s.as_ref()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl Default for $T {
            fn default() -> Self {
                Self(Arc::from(""))
            }
        }

        impl fmt::Display for $T {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $T {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $T {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $T {
            fn from(s: String) -> Self {
                Self(Arc::from(s.as_str()))
            }
        }

        impl From<&str> for $T {
            fn from(s: &str) -> Self {
                Self(Arc::from(s))
            }
        }

        impl From<$T> for String {
            fn from(id: $T) -> Self {
                id.0.to_string()
            }
        }

        impl PartialEq<str> for $T {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $T {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl PartialEq<String> for $T {
            fn eq(&self, other: &String) -> bool {
                self.as_str() == other.as_str()
            }
        }

        impl Deref for $T {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<$T> for String {
            fn eq(&self, other: &$T) -> bool {
                self.as_str() == other.as_str()
            }
        }

        impl PartialEq<$T> for &str {
            fn eq(&self, other: &$T) -> bool {
                *self == other.as_str()
            }
        }
    };
}

impl_id_type!(ClipId);
impl_id_type!(LayerId);
impl_id_type!(EffectGroupId);
impl_id_type!(EffectId);
impl_id_type!(MarkerId);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn clone_shares_allocation() {
        let id = ClipId::new("abc123");
        let cloned = id.clone();
        // Arc::clone shares the same pointer — no new heap allocation.
        assert!(Arc::ptr_eq(&id.0, &cloned.0));
    }

    #[test]
    fn equality_across_separate_allocations() {
        let a = ClipId::new("same");
        let b = ClipId::new("same");
        assert_eq!(a, b);
        // Different allocations — not pointer-equal but value-equal.
        assert!(!Arc::ptr_eq(&a.0, &b.0));
    }

    #[test]
    fn hashmap_lookup_with_str() {
        let mut map = HashMap::new();
        let id = LayerId::new("layer-1");
        map.insert(id.clone(), 42);
        // Borrow<str> allows lookup with &str key.
        assert_eq!(map.get("layer-1"), Some(&42));
    }

    #[test]
    fn ahashmap_lookup_with_str() {
        use ahash::AHashMap;
        let mut map = AHashMap::new();
        let id = ClipId::new("clip-1");
        map.insert(id.clone(), 99);
        assert_eq!(map.get("clip-1"), Some(&99));
    }

    #[test]
    fn serde_round_trip() {
        let id = ClipId::new("test-id-42");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"test-id-42\"");
        let deserialized: ClipId = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, id);
    }

    #[test]
    fn serde_round_trip_in_struct() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            id: ClipId,
            layer: LayerId,
        }
        let w = Wrapper {
            id: ClipId::new("c1"),
            layer: LayerId::new("l1"),
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn default_is_empty() {
        assert!(ClipId::default().is_empty());
        assert!(LayerId::default().is_empty());
        assert!(EffectGroupId::default().is_empty());
        assert!(EffectId::default().is_empty());
        assert!(MarkerId::default().is_empty());
    }

    #[test]
    fn display() {
        let id = ClipId::new("hello");
        assert_eq!(format!("{id}"), "hello");
    }

    #[test]
    fn from_string() {
        let s = String::from("from-string");
        let id = ClipId::from(s);
        assert_eq!(id.as_str(), "from-string");
    }

    #[test]
    fn from_str_ref() {
        let id = LayerId::from("from-ref");
        assert_eq!(id.as_str(), "from-ref");
    }

    #[test]
    fn into_string() {
        let id = EffectGroupId::new("eg-1");
        let s: String = id.into();
        assert_eq!(s, "eg-1");
    }

    #[test]
    fn partial_eq_str() {
        let id = ClipId::new("x");
        assert!(id == "x");
        assert!(id == *"x");
        assert!(id == String::from("x"));
        assert!(String::from("x") == id);
        let s: &str = "x";
        assert!(s == ClipId::new("x"));
    }

    #[test]
    fn deref_to_str() {
        let id = ClipId::new("deref-test");
        let s: &str = &id;
        assert_eq!(s, "deref-test");
        // String methods available via Deref
        assert!(id.starts_with("deref"));
        assert_eq!(id.len(), 10);
    }

    #[test]
    fn as_ref_str() {
        let id = LayerId::new("ref-test");
        fn takes_as_ref(s: &impl AsRef<str>) -> &str {
            s.as_ref()
        }
        assert_eq!(takes_as_ref(&id), "ref-test");
    }

    #[test]
    fn borrow_str() {
        let id = ClipId::new("borrow-test");
        let borrowed: &str = id.borrow();
        assert_eq!(borrowed, "borrow-test");
    }

    #[test]
    fn all_five_types_work() {
        // Smoke test that the macro works for all 5 types.
        let c = ClipId::new("c");
        let l = LayerId::new("l");
        let eg = EffectGroupId::new("eg");
        let e = EffectId::new("e");
        let m = MarkerId::new("m");
        assert_eq!(c.as_str(), "c");
        assert_eq!(l.as_str(), "l");
        assert_eq!(eg.as_str(), "eg");
        assert_eq!(e.as_str(), "e");
        assert_eq!(m.as_str(), "m");
        // Clone all
        let _ = (c.clone(), l.clone(), eg.clone(), e.clone(), m.clone());
    }
}
