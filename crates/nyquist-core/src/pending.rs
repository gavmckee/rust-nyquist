/// Bounded, in-memory delivery buffer. The caller clears only after a
/// successful insert; timestamps belong to the entries, not the retry time.
pub struct Pending<T> {
    entries: Vec<T>,
    capacity: usize,
    dropped: u64,
}

impl<T> Pending<T> {
    pub fn new(capacity: usize) -> Self {
        Self { entries: Vec::new(), capacity, dropped: 0 }
    }
    pub fn push(&mut self, entry: T) {
        if self.entries.len() < self.capacity {
            self.entries.push(entry);
        } else {
            self.dropped = self.dropped.saturating_add(1);
            tracing::warn!(dropped_total = self.dropped, capacity = self.capacity,
                "configuration event buffer full; dropping newest event");
        }
    }
    pub fn entries(&self) -> &[T] { &self.entries }
    pub fn acknowledge(&mut self) { self.entries.clear(); }
    pub fn dropped(&self) -> u64 { self.dropped }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_retains_order_timestamps_and_bounds_memory() {
        let mut p = Pending::new(2);
        p.push((100, "old->new"));
        // Failed delivery: no acknowledgement. A later change is retained too.
        p.push((200, "new->old"));
        p.push((300, "overflow"));
        assert_eq!(p.entries(), &[(100, "old->new"), (200, "new->old")]);
        assert_eq!(p.dropped(), 1);
        p.acknowledge();
        assert!(p.entries().is_empty());
        p.push((400, "recovered"));
        assert_eq!(p.entries(), &[(400, "recovered")]);
    }
}
