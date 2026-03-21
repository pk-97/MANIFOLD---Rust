// Typed identifiers for compile-time safety.
//
// Prevents mixing clip IDs, layer IDs, and effect group IDs at the type level.
// All three wrap `String` with serde(transparent) for backwards-compatible JSON.

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

/// Clip identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct ClipId(pub String);

/// Layer identifier — wraps a short UUID string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct LayerId(pub String);

/// Effect group identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct EffectGroupId(pub String);

// ── Macro for shared impls ──

macro_rules! impl_id_type {
    ($T:ident) => {
        impl $T {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
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
                Self(s)
            }
        }

        impl From<&str> for $T {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<$T> for String {
            fn from(id: $T) -> Self {
                id.0
            }
        }

        impl PartialEq<str> for $T {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $T {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $T {
            fn eq(&self, other: &String) -> bool {
                self.0 == *other
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
                *self == other.0
            }
        }

        impl PartialEq<$T> for &str {
            fn eq(&self, other: &$T) -> bool {
                *self == other.0.as_str()
            }
        }
    };
}

impl_id_type!(ClipId);
impl_id_type!(LayerId);
impl_id_type!(EffectGroupId);
