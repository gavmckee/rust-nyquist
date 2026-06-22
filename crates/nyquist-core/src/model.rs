use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind { Counter, Gauge, Distribution }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit { Count, Bytes, Seconds, Percent, None }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Labels(BTreeMap<String, String>);

impl Labels {
    pub fn new() -> Self { Labels(BTreeMap::new()) }
    pub fn insert(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.0.insert(k.into(), v.into());
        self
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> { self.0.iter() }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetricId(pub u64);

pub fn metric_id(name: &str, labels: &Labels) -> MetricId {
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    for (k, v) in labels.iter() {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    MetricId(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_id_is_stable_and_label_order_independent() {
        let a = Labels::new().insert("cpu", "0").insert("core", "1");
        let b = Labels::new().insert("core", "1").insert("cpu", "0");
        assert_eq!(metric_id("cpu/usage", &a), metric_id("cpu/usage", &b));
        assert_ne!(metric_id("cpu/usage", &a), metric_id("cpu/idle", &a));
    }

    #[test]
    fn labels_iterate_sorted() {
        let l = Labels::new().insert("z", "1").insert("a", "2");
        let keys: Vec<_> = l.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "z"]);
    }
}
