//! Retained per-target acceleration fields parallel to the rigid input history.
//! A target is a body slot, then a copy group (`bodies + group`). Only the
//! targets that have carried a field since the world was built hold a
//! column, so a big world with one wired force keeps one column of history.

use manifold_physics::{FieldValue, input::HistoryWrite};

use super::body_history::PackedRows;
use crate::vector_field::ContinuousField;

const HISTORY_CAPACITY: usize = super::AUTHORED_HISTORY_CAPACITY;

/// One sample's targeted fields: a field per target, or none anywhere.
#[derive(Clone, Copy)]
pub(crate) enum TargetedFields<'a> {
    Values(&'a [Option<FieldValue>]),
    Cleared,
}

pub(crate) struct TargetedFieldHistory {
    samples: PackedRows<Option<FieldValue>>,
    /// Column `c` holds target `columns[c]`.
    columns: Vec<usize>,
    /// Each target's column, if it has carried a field.
    column_of: Vec<Option<usize>>,
    connected: bool,
}

impl Default for TargetedFieldHistory {
    fn default() -> Self {
        Self { samples: PackedRows::new(None), columns: Vec::new(), column_of: Vec::new(), connected: false }
    }
}

impl TargetedFieldHistory {
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Size for `targets` targets and forget every sample and column. The
    /// world calls this when it rebuilds, so no sample is lost.
    pub fn set_targets(&mut self, targets: usize) {
        self.clear();
        self.columns.clear();
        self.column_of.clear();
        self.column_of.resize(targets, None);
        self.samples.configure(0, HISTORY_CAPACITY);
    }

    #[cfg(test)]
    pub fn columns(&self) -> usize {
        self.columns.len()
    }

    #[cfg(test)]
    pub fn storage_ptr(&self) -> usize {
        self.samples.storage_ptr()
    }

    pub fn ensure_aligned(&mut self, len: usize) -> Result<(), String> {
        if self.connected {
            return Ok(());
        }
        if len > HISTORY_CAPACITY {
            return Err("Physics: targeted field history cannot align authored samples".into());
        }
        self.samples.clear();
        for _ in 0..len {
            self.samples
                .record_filler(HistoryWrite::Appended)
                .map_err(|error| format!("Physics: targeted field history {error}"))?;
        }
        self.connected = true;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.connected = false;
    }

    pub fn record(&mut self, fields: TargetedFields<'_>, write: HistoryWrite) -> Result<(), String> {
        if !self.connected {
            return Err("Physics: targeted field history is not connected".into());
        }
        let result = match fields {
            TargetedFields::Values(fields) => {
                if fields.len() != self.column_of.len() {
                    return Err(format!(
                        "Physics: targeted fields require exactly {} entries",
                        self.column_of.len()
                    ));
                }
                // A target's first field opens its column: wiring a force is
                // an authoring edit, so this widening never runs in a
                // steady tick.
                let before = self.columns.len();
                for (target, field) in fields.iter().enumerate() {
                    if field.is_some() && self.column_of[target].is_none() {
                        self.column_of[target] = Some(self.columns.len());
                        self.columns.push(target);
                    }
                }
                if self.columns.len() != before {
                    self.samples.widen(self.columns.len());
                }
                let columns = &self.columns;
                self.samples.record(write, columns.iter().map(|&target| &fields[target]))
            }
            TargetedFields::Cleared => self.samples.record_filler(write),
        };
        result.map_err(|error| format!("Physics: targeted field history {error}"))
    }

    pub fn prune(&mut self, count: usize) -> Result<(), String> {
        if !self.connected {
            return Ok(());
        }
        if count >= self.samples.len() {
            return Err("Physics: targeted field history lost alignment".into());
        }
        self.samples
            .prune(count)
            .map_err(|error| format!("Physics: targeted field history {error}"))
    }

    pub fn span(
        &self,
        before_index: usize,
        after_index: usize,
        alpha: f32,
    ) -> Result<TargetedFieldSpan<'_>, String> {
        if !self.connected {
            return Err("Physics: targeted field history is not connected".into());
        }
        let before = self
            .samples
            .row(before_index)
            .ok_or("Physics: targeted field history span is out of bounds")?;
        let after = self
            .samples
            .row(after_index)
            .ok_or("Physics: targeted field history span is out of bounds")?;
        Ok(TargetedFieldSpan {
            before,
            after,
            column_of: &self.column_of,
            alpha,
        })
    }

    pub fn fields_equal(&self, fields: TargetedFields<'_>) -> bool {
        self.samples.back().is_some_and(|sample| match fields {
            TargetedFields::Values(fields) => {
                fields.len() == self.column_of.len()
                    && fields.iter().zip(&self.column_of).all(|(field, column)| match column {
                        Some(column) => sample[*column] == *field,
                        None => field.is_none(),
                    })
            }
            TargetedFields::Cleared => sample.iter().all(Option::is_none),
        })
    }
}

pub(crate) struct TargetedFieldSpan<'a> {
    before: &'a [Option<FieldValue>],
    after: &'a [Option<FieldValue>],
    column_of: &'a [Option<usize>],
    alpha: f32,
}

impl TargetedFieldSpan<'_> {
    pub fn field(&self, target: usize) -> ContinuousField<'_> {
        let column = self.column_of.get(target).copied().flatten();
        ContinuousField {
            before: column.and_then(|column| self.before[column].as_ref()),
            after: column.and_then(|column| self.after[column].as_ref()),
            alpha: self.alpha,
            origin: [0.0; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_world_with_one_wired_force_keeps_one_column() {
        let mut history = TargetedFieldHistory::default();
        history.set_targets(288);
        history.ensure_aligned(1).unwrap();
        let mut fields = vec![None; 288];
        fields[200] = Some(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap());
        history.record(TargetedFields::Values(&fields), HistoryWrite::Appended).unwrap();
        assert_eq!(history.columns, [200]);
        assert!(history.fields_equal(TargetedFields::Values(&fields)));
        let span = history.span(1, 1, 0.0).unwrap();
        assert!(span.field(200).after.is_some());
        assert!(span.field(3).after.is_none());
        let before = history.span(0, 0, 0.0).unwrap();
        assert!(before.field(200).before.is_none(), "the aligned row before the force has none");
        let storage = history.storage_ptr();
        history.record(TargetedFields::Values(&fields), HistoryWrite::Appended).unwrap();
        assert_eq!(history.storage_ptr(), storage, "a steady force never widens again");
    }
}
