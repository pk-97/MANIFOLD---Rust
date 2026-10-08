//! Retained per-target acceleration fields parallel to the rigid input history.

use std::collections::VecDeque;

use manifold_physics::{FieldValue, input::HistoryWrite};

use crate::scene::vector_field::ContinuousField;

pub const TARGET_SLOTS: usize = super::MAX_BODIES + 1;
const HISTORY_CAPACITY: usize = super::AUTHORED_HISTORY_CAPACITY;

#[derive(Default)]
pub(crate) struct TargetedFieldHistory {
    samples: VecDeque<[Option<FieldValue>; TARGET_SLOTS]>,
    connected: bool,
}

impl TargetedFieldHistory {
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.samples.capacity()
    }

    #[cfg(test)]
    pub fn storage_ptr(&self) -> usize {
        let (front, back) = self.samples.as_slices();
        // The second physical slice starts at the allocation base, including
        // when empty. Taking the lower address ignores deque head movement.
        (front.as_ptr() as usize).min(back.as_ptr() as usize)
    }

    pub fn ensure_aligned(&mut self, len: usize) -> Result<(), String> {
        if self.connected {
            return Ok(());
        }
        if len > HISTORY_CAPACITY {
            return Err("Physics: targeted field history cannot align authored samples".into());
        }
        self.samples.clear();
        if self.samples.capacity() < HISTORY_CAPACITY {
            self.samples.reserve_exact(HISTORY_CAPACITY);
        }
        for _ in 0..len {
            self.samples.push_back(empty_fields());
        }
        self.connected = true;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.connected = false;
    }

    pub fn record(
        &mut self,
        fields: &[Option<FieldValue>],
        write: HistoryWrite,
    ) -> Result<(), String> {
        if fields.len() != TARGET_SLOTS {
            return Err(format!(
                "Physics: targeted fields require exactly {TARGET_SLOTS} entries"
            ));
        }
        if !self.connected {
            return Err("Physics: targeted field history is not connected".into());
        }
        match write {
            HistoryWrite::Appended => {
                if self.samples.len() == HISTORY_CAPACITY {
                    return Err("Physics: targeted field history lost capacity alignment".into());
                }
                self.samples.push_back(clone_fields(fields));
            }
            HistoryWrite::Replaced => {
                let Some(last) = self.samples.back_mut() else {
                    return Err("Physics: targeted field history is empty".into());
                };
                *last = clone_fields(fields);
            }
        }
        Ok(())
    }

    pub fn prune(&mut self, count: usize) -> Result<(), String> {
        if !self.connected {
            return Ok(());
        }
        if count >= self.samples.len() {
            return Err("Physics: targeted field history lost alignment".into());
        }
        for _ in 0..count {
            self.samples.pop_front();
        }
        Ok(())
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
            .get(before_index)
            .ok_or("Physics: targeted field history span is out of bounds")?;
        let after = self
            .samples
            .get(after_index)
            .ok_or("Physics: targeted field history span is out of bounds")?;
        Ok(TargetedFieldSpan {
            before,
            after,
            alpha,
        })
    }

    pub fn fields_equal(&self, fields: &[Option<FieldValue>]) -> bool {
        self.samples
            .back()
            .is_some_and(|sample| sample.as_slice() == fields)
    }
}

pub(crate) struct TargetedFieldSpan<'a> {
    before: &'a [Option<FieldValue>; TARGET_SLOTS],
    after: &'a [Option<FieldValue>; TARGET_SLOTS],
    alpha: f32,
}

impl TargetedFieldSpan<'_> {
    pub fn field(&self, index: usize) -> ContinuousField<'_> {
        ContinuousField {
            before: self.before[index].as_ref(),
            after: self.after[index].as_ref(),
            alpha: self.alpha,
            origin: [0.0; 3],
        }
    }
}

fn empty_fields() -> [Option<FieldValue>; TARGET_SLOTS] {
    std::array::from_fn(|_| None)
}

fn clone_fields(fields: &[Option<FieldValue>]) -> [Option<FieldValue>; TARGET_SLOTS] {
    std::array::from_fn(|index| fields[index].clone())
}
