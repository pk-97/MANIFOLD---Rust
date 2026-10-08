//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{ExtentRule, texture_only};

inventory::submit! {
    ExtentRule { type_id: "node.exposure", check: texture_only }
}
