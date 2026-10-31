//! First-fit range allocator for the shared GPU quad buffer.

use std::collections::BTreeMap;

pub struct RangeAllocator {
    capacity: u32,
    used: u32,
    /// Free ranges by start offset.
    free: BTreeMap<u32, u32>,
}

impl RangeAllocator {
    pub fn new(capacity: u32) -> Self {
        let mut free = BTreeMap::new();
        free.insert(0, capacity);
        Self {
            capacity,
            used: 0,
            free,
        }
    }

    pub fn clear(&mut self) {
        self.used = 0;
        self.free.clear();
        self.free.insert(0, self.capacity);
    }

    pub fn used(&self) -> u32 {
        self.used
    }

    pub fn alloc(&mut self, count: u32) -> Option<u32> {
        let (&start, &size) = self.free.iter().find(|&(_, &size)| size >= count)?;
        self.free.remove(&start);
        if size > count {
            self.free.insert(start + count, size - count);
        }
        self.used += count;
        Some(start)
    }

    pub fn free(&mut self, start: u32, count: u32) {
        self.used -= count;
        let mut start = start;
        let mut count = count;
        if let Some((&ps, &pc)) = self.free.range(..start).next_back() {
            debug_assert!(ps + pc <= start, "double free");
            if ps + pc == start {
                self.free.remove(&ps);
                start = ps;
                count += pc;
            }
        }
        if let Some(&nc) = self.free.get(&(start + count)) {
            self.free.remove(&(start + count));
            count += nc;
        }
        self.free.insert(start, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frees_merge_back_into_a_single_range() {
        let mut a = RangeAllocator::new(100);
        let x = a.alloc(30).unwrap();
        let y = a.alloc(30).unwrap();
        let z = a.alloc(40).unwrap();
        assert!(a.alloc(1).is_none());
        a.free(y, 30);
        assert_eq!(a.used(), 70);
        a.free(x, 30);
        a.free(z, 40);
        assert_eq!(a.used(), 0);
        assert_eq!(a.alloc(100), Some(0));
    }

    #[test]
    fn reuses_a_gap_that_fits() {
        let mut a = RangeAllocator::new(100);
        let _ = a.alloc(10);
        let b = a.alloc(20).unwrap();
        let _ = a.alloc(10);
        a.free(b, 20);
        assert_eq!(a.alloc(15), Some(b));
        assert_eq!(a.alloc(5), Some(b + 15));
    }
}
