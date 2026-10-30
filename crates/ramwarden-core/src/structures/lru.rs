//! Least-recently-used ordering: a hash map paired with a doubly linked list.
//!
//! ```text
//!   map: key ──► slot
//!
//!   head (hottest)                                   tail (coldest)
//!     ┌───┐ ◄──► ┌───┐ ◄──► ┌───┐ ◄──► ┌───┐
//!     │ A │      │ B │      │ C │      │ D │   ◄── evict / flag from here
//!     └───┘      └───┘      └───┘      └───┘
//! ```
//!
//! Touching an entry looks it up in the map, unlinks it from wherever it sits
//! and relinks it at the head: O(1) regardless of size, which is the point. The
//! entry that has gone longest without a touch is always at the tail, so
//! "what is coldest?" is also O(1) and "the N coldest" is O(N). The kernel's
//! active/inactive page lists work the same way.
//!
//! The list lives in a `Vec` of slots and links by index rather than by pointer.
//! That keeps it in safe Rust (a doubly linked list of `Box`es would need two
//! owners per node), and freed slots are reused so long-running churn does not
//! grow it.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

struct Node<K, V> {
    key: K,
    value: V,
    /// Towards the head (hotter).
    prev: Option<usize>,
    /// Towards the tail (colder).
    next: Option<usize>,
}

pub struct Lru<K, V> {
    map: HashMap<K, usize>,
    slots: Vec<Option<Node<K, V>>>,
    free: Vec<usize>,
    head: Option<usize>,
    tail: Option<usize>,
}

impl<K: Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new() -> Self {
        Lru {
            map: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(key)
    }

    /// Read without counting as a touch.
    pub fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = *self.map.get(key)?;
        Some(&self.node(i).value)
    }

    /// Modify without counting as a touch.
    pub fn peek_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = *self.map.get(key)?;
        Some(&mut self.node_mut(i).value)
    }

    /// Move `key` to the head. Returns its value, or `None` if absent.
    pub fn touch<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = *self.map.get(key)?;
        self.unlink(i);
        self.link_front(i);
        Some(&mut self.node_mut(i).value)
    }

    /// Insert at the head, or replace the value and move it there.
    pub fn put_hot(&mut self, key: K, value: V) {
        if let Some(&i) = self.map.get(&key) {
            self.node_mut(i).value = value;
            self.unlink(i);
            self.link_front(i);
            return;
        }
        let i = self.alloc(key, value);
        self.link_front(i);
    }

    /// Insert at the tail, as if it had never been touched.
    ///
    /// An existing entry keeps its place and only has its value replaced:
    /// arriving again is not the same as being used.
    pub fn put_cold(&mut self, key: K, value: V) {
        if let Some(&i) = self.map.get(&key) {
            self.node_mut(i).value = value;
            return;
        }
        let i = self.alloc(key, value);
        self.link_back(i);
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.map.remove(key)?;
        self.unlink(i);
        let node = self.slots[i].take().expect("mapped slot is occupied");
        self.free.push(i);
        Some(node.value)
    }

    /// The least recently touched entry.
    pub fn coldest(&self) -> Option<(&K, &V)> {
        self.tail.map(|i| {
            let n = self.node(i);
            (&n.key, &n.value)
        })
    }

    /// The most recently touched entry.
    pub fn hottest(&self) -> Option<(&K, &V)> {
        self.head.map(|i| {
            let n = self.node(i);
            (&n.key, &n.value)
        })
    }

    /// Remove and return the least recently touched entry.
    pub fn pop_coldest(&mut self) -> Option<(K, V)> {
        let i = self.tail?;
        let key = self.node(i).key.clone();
        let value = self.remove(&key)?;
        Some((key, value))
    }

    /// Coldest first: the order to consider things for closing.
    pub fn iter_cold(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut cur = self.tail;
        std::iter::from_fn(move || {
            let n = self.node(cur?);
            cur = n.prev;
            Some((&n.key, &n.value))
        })
    }

    /// Hottest first.
    pub fn iter_hot(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut cur = self.head;
        std::iter::from_fn(move || {
            let n = self.node(cur?);
            cur = n.next;
            Some((&n.key, &n.value))
        })
    }

    /// Drop every entry for which `keep` is false, preserving the order of the rest.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        let gone: Vec<K> = self
            .iter_hot()
            .filter(|(k, v)| !keep(k, v))
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            self.remove(&k);
        }
    }

    // ── links ───────────────────────────────────────────────────────────────

    fn node(&self, i: usize) -> &Node<K, V> {
        self.slots[i].as_ref().expect("linked slot is occupied")
    }

    fn node_mut(&mut self, i: usize) -> &mut Node<K, V> {
        self.slots[i].as_mut().expect("linked slot is occupied")
    }

    fn alloc(&mut self, key: K, value: V) -> usize {
        let node = Node {
            key: key.clone(),
            value,
            prev: None,
            next: None,
        };
        let i = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(node);
                i
            }
            None => {
                self.slots.push(Some(node));
                self.slots.len() - 1
            }
        };
        self.map.insert(key, i);
        i
    }

    /// Take `i` out of the list, joining its neighbours.
    fn unlink(&mut self, i: usize) {
        let (prev, next) = {
            let n = self.node(i);
            (n.prev, n.next)
        };
        match prev {
            Some(p) => self.node_mut(p).next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.node_mut(n).prev = prev,
            None => self.tail = prev,
        }
        let n = self.node_mut(i);
        n.prev = None;
        n.next = None;
    }

    fn link_front(&mut self, i: usize) {
        let old = self.head;
        {
            let n = self.node_mut(i);
            n.prev = None;
            n.next = old;
        }
        match old {
            Some(h) => self.node_mut(h).prev = Some(i),
            None => self.tail = Some(i),
        }
        self.head = Some(i);
    }

    fn link_back(&mut self, i: usize) {
        let old = self.tail;
        {
            let n = self.node_mut(i);
            n.next = None;
            n.prev = old;
        }
        match old {
            Some(t) => self.node_mut(t).next = Some(i),
            None => self.head = Some(i),
        }
        self.tail = Some(i);
    }
}

impl<K: Hash + Eq + Clone, V> Default for Lru<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Hash + Eq + Clone + std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for Lru<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter_hot()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(l: &Lru<&'static str, u32>) -> Vec<&'static str> {
        l.iter_hot().map(|(k, _)| *k).collect()
    }

    #[test]
    fn touching_moves_to_the_front_and_the_tail_is_coldest() {
        let mut l = Lru::new();
        l.put_hot("a", 1);
        l.put_hot("b", 2);
        l.put_hot("c", 3);
        assert_eq!(order(&l), vec!["c", "b", "a"]);
        assert_eq!(l.coldest(), Some((&"a", &1)));

        assert_eq!(l.touch("a").copied(), Some(1));
        assert_eq!(order(&l), vec!["a", "c", "b"]);
        assert_eq!(l.coldest(), Some((&"b", &2)));
        assert_eq!(l.hottest(), Some((&"a", &1)));

        // Touching the head is a no-op on order.
        l.touch("a");
        assert_eq!(order(&l), vec!["a", "c", "b"]);
        assert!(l.touch("missing").is_none());
    }

    #[test]
    fn cold_iteration_runs_tail_to_head() {
        let mut l = Lru::new();
        for (k, v) in [("a", 1), ("b", 2), ("c", 3)] {
            l.put_hot(k, v);
        }
        let cold: Vec<_> = l.iter_cold().map(|(k, _)| *k).collect();
        assert_eq!(cold, vec!["a", "b", "c"]);
    }

    #[test]
    fn put_cold_joins_at_the_tail_and_keeps_an_existing_place() {
        let mut l = Lru::new();
        l.put_hot("a", 1);
        l.put_cold("b", 2);
        assert_eq!(order(&l), vec!["a", "b"]);
        l.put_cold("a", 10);
        assert_eq!(order(&l), vec!["a", "b"]);
        assert_eq!(l.peek("a"), Some(&10));
    }

    #[test]
    fn remove_and_pop_relink_neighbours() {
        let mut l = Lru::new();
        for (k, v) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
            l.put_hot(k, v);
        }
        assert_eq!(l.remove("c"), Some(3)); // middle
        assert_eq!(order(&l), vec!["d", "b", "a"]);
        assert_eq!(l.remove("d"), Some(4)); // head
        assert_eq!(order(&l), vec!["b", "a"]);
        assert_eq!(l.pop_coldest(), Some(("a", 1))); // tail
        assert_eq!(order(&l), vec!["b"]);
        assert_eq!(l.pop_coldest(), Some(("b", 2)));
        assert!(l.is_empty());
        assert_eq!(l.coldest(), None);
        assert_eq!(l.pop_coldest(), None);
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut l = Lru::new();
        for round in 0..100u32 {
            l.put_hot("x", round);
            l.put_hot("y", round);
            l.remove("x");
            l.remove("y");
        }
        assert!(l.slots.len() <= 2);
    }

    #[test]
    fn retain_keeps_order() {
        let mut l = Lru::new();
        for (k, v) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
            l.put_hot(k, v);
        }
        l.retain(|_, v| v % 2 == 0);
        assert_eq!(order(&l), vec!["d", "b"]);
    }

    #[test]
    fn borrowed_lookups_work_for_owned_keys() {
        let mut l: Lru<String, u32> = Lru::new();
        l.put_hot("Discord".to_string(), 1);
        assert!(l.contains("Discord"));
        assert!(l.touch("Discord").is_some());
        assert_eq!(l.remove("Discord"), Some(1));
    }
}
