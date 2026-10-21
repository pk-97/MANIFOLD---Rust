//! Fixed rows parallel to the rigid input history: one row per authored
//! sample, one cell per body then per copy group. Storage is sized when the
//! world's counts change, so recording a sample never allocates.

use manifold_physics::input::HistoryWrite;

pub(crate) struct PackedRows<T> {
    cells: Vec<T>,
    filler: T,
    stride: usize,
    capacity: usize,
    head: usize,
    len: usize,
}

impl<T: Clone> PackedRows<T> {
    pub fn new(filler: T) -> Self {
        Self { cells: Vec::new(), filler, stride: 0, capacity: 0, head: 0, len: 0 }
    }

    /// Hold `capacity` rows of `stride` cells and forget every row. Storage
    /// is reallocated only when it must grow.
    pub fn configure(&mut self, stride: usize, capacity: usize) {
        let cells = stride * capacity;
        self.cells.clear();
        self.cells.resize(cells, self.filler.clone());
        self.stride = stride;
        self.capacity = capacity;
        self.head = 0;
        self.len = 0;
    }

    /// Grow every row to `stride` cells, keeping each row's cells in their
    /// columns and filling the new ones.
    pub fn widen(&mut self, stride: usize) {
        if stride <= self.stride {
            return;
        }
        let mut wider = Self::new(self.filler.clone());
        wider.configure(stride, self.capacity);
        for index in 0..self.len {
            let from = self.physical(index);
            let to = index * stride;
            wider.cells[to..to + self.stride].clone_from_slice(&self.cells[from..from + self.stride]);
        }
        wider.len = self.len;
        *self = wider;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn clear(&mut self) {
        for index in 0..self.len {
            let row = self.physical(index);
            self.cells[row..row + self.stride].fill(self.filler.clone());
        }
        self.head = 0;
        self.len = 0;
    }

    fn physical(&self, index: usize) -> usize {
        ((self.head + index) % self.capacity) * self.stride
    }

    /// Mirror one history write: `Appended` adds a row, `Replaced`
    /// overwrites the newest one. `values` yields exactly one value per cell.
    pub fn record<'a>(
        &mut self,
        write: HistoryWrite,
        values: impl IntoIterator<Item = &'a T>,
    ) -> Result<(), &'static str>
    where
        T: 'a,
    {
        let index = match write {
            HistoryWrite::Appended => {
                if self.len == self.capacity {
                    return Err("lost capacity alignment");
                }
                self.len += 1;
                self.len - 1
            }
            HistoryWrite::Replaced => self.len.checked_sub(1).ok_or("has no row to replace")?,
        };
        let row = self.physical(index);
        let mut written = 0;
        for (cell, value) in self.cells[row..row + self.stride].iter_mut().zip(values) {
            cell.clone_from(value);
            written += 1;
        }
        if written != self.stride {
            return Err("received a row of the wrong width");
        }
        Ok(())
    }

    /// Mirror a row written as all filler (no value in any cell).
    pub fn record_filler(&mut self, write: HistoryWrite) -> Result<(), &'static str> {
        let index = match write {
            HistoryWrite::Appended => {
                if self.len == self.capacity {
                    return Err("lost capacity alignment");
                }
                self.len += 1;
                self.len - 1
            }
            HistoryWrite::Replaced => self.len.checked_sub(1).ok_or("has no row to replace")?,
        };
        let row = self.physical(index);
        self.cells[row..row + self.stride].fill(self.filler.clone());
        Ok(())
    }

    /// Drop the oldest `count` rows, as the history pruned them.
    pub fn prune(&mut self, count: usize) -> Result<(), &'static str> {
        if count > self.len {
            return Err("lost alignment");
        }
        for _ in 0..count {
            let row = self.physical(0);
            self.cells[row..row + self.stride].fill(self.filler.clone());
            self.head = (self.head + 1) % self.capacity.max(1);
            self.len -= 1;
        }
        Ok(())
    }

    pub fn row(&self, index: usize) -> Option<&[T]> {
        (index < self.len).then(|| {
            let row = self.physical(index);
            &self.cells[row..row + self.stride]
        })
    }

    pub fn back(&self) -> Option<&[T]> {
        self.len.checked_sub(1).and_then(|index| self.row(index))
    }

    #[cfg(test)]
    pub fn storage_ptr(&self) -> usize {
        self.cells.as_ptr() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_appends_replacements_and_prunes_without_reallocating() {
        let mut rows = PackedRows::new(None::<u32>);
        rows.configure(3, 2);
        let storage = rows.storage_ptr();
        rows.record(HistoryWrite::Appended, &[Some(1), None, Some(3)]).unwrap();
        rows.record(HistoryWrite::Appended, &[Some(4), Some(5), None]).unwrap();
        assert!(rows.record(HistoryWrite::Appended, &[None, None, None]).is_err());
        rows.record(HistoryWrite::Replaced, &[Some(7), Some(8), Some(9)]).unwrap();
        assert_eq!(rows.row(0), Some(&[Some(1), None, Some(3)][..]));
        assert_eq!(rows.back(), Some(&[Some(7), Some(8), Some(9)][..]));
        rows.prune(1).unwrap();
        assert_eq!(rows.row(0), Some(&[Some(7), Some(8), Some(9)][..]));
        rows.record_filler(HistoryWrite::Appended).unwrap();
        assert_eq!(rows.back(), Some(&[None, None, None][..]));
        assert!(rows.record(HistoryWrite::Replaced, &[None, None]).is_err(), "a short row is refused");
        assert_eq!(rows.storage_ptr(), storage);
    }
}
