//! Retain only K best values; the heap root is the worst retained candidate.
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

pub type Tie = (u8, u64, String, String);

struct Item<T> {
    key: (Reverse<i128>, Tie),
    value: T,
}

impl<T> PartialEq for Item<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl<T> Eq for Item<T> {}
impl<T> PartialOrd for Item<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Item<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

pub struct Top<T> {
    limit: usize,
    heap: BinaryHeap<Item<T>>,
}

impl<T> Top<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
        }
    }

    pub fn offer(&mut self, score: i128, tie: Tie, value: T) {
        let item = Item {
            key: (Reverse(score), tie),
            value,
        };
        if self.heap.len() < self.limit {
            self.heap.push(item);
        } else if self.heap.peek().is_some_and(|worst| item < *worst) {
            *self.heap.peek_mut().unwrap() = item;
        }
    }

    pub fn finish(self) -> Vec<T> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|item| item.value)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_better_ties_replace_worst_candidates() {
        let mut top = Top::new(2);
        for (score, path) in [(2, "z"), (3, "b"), (3, "a"), (1, "x")] {
            top.offer(score, (0, 0, path.into(), String::new()), path);
        }
        assert_eq!(top.finish(), ["a", "b"]);
    }
}
