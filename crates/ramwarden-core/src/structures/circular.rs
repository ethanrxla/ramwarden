//! A circularly linked list, for taking turns.
//!
//! ```text
//!          ┌──────────────────────────────┐
//!          ▼                              │
//!   head ┌───┐ ──► ┌───┐ ──► ┌───┐ ──► ┌───┐ tail
//!        │ A │     │ B │     │ C │     │ D │
//!        └───┘     └───┘     └───┘     └───┘
//! ```
//!
//! Only the tail is stored; the head is always `tail.next`. Rotating moves the
//! tail one step, so the old head goes to the back of the line in O(1) without
//! unlinking anything. That makes it a round-robin: call [`CircularList::turn`]
//! repeatedly and every element comes up in order, forever.
//!
//! Nodes live in a `Vec` and link by index, like [`super::Lru`], which keeps it
//! in safe Rust; a cycle of `Box`es cannot have a single owner.

struct Node<T> {
    element: T,
    next: usize,
}

pub struct CircularList<T> {
    slots: Vec<Option<Node<T>>>,
    free: Vec<usize>,
    tail: Option<usize>,
    size: usize,
}

impl<T> CircularList<T> {
    pub fn new() -> Self {
        CircularList {
            slots: Vec::new(),
            free: Vec::new(),
            tail: None,
            size: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The element whose turn is next.
    pub fn first(&self) -> Option<&T> {
        let t = self.tail?;
        Some(&self.node(self.node(t).next).element)
    }

    /// Move the front element to the back.
    pub fn rotate(&mut self) {
        if let Some(t) = self.tail {
            self.tail = Some(self.node(t).next);
        }
    }

    /// Take the next turn: the front element, which then goes to the back.
    pub fn turn(&mut self) -> Option<&T> {
        self.rotate();
        self.tail.map(|t| &self.node(t).element)
    }

    /// Add `e` at the back of the line, just before the current front.
    pub fn insert_last(&mut self, e: T) {
        let i = self.alloc(e);
        match self.tail {
            None => self.node_mut(i).next = i, // a single node points at itself
            Some(t) => {
                let head = self.node(t).next;
                self.node_mut(i).next = head;
                self.node_mut(t).next = i;
            }
        }
        self.tail = Some(i);
        self.size += 1;
    }

    /// Remove the first element matching `pred`. O(n): a singly linked list
    /// has to walk to find the predecessor.
    pub fn remove_where(&mut self, mut pred: impl FnMut(&T) -> bool) -> Option<T> {
        let t = self.tail?;
        let mut prev = t;
        for _ in 0..self.size {
            let cur = self.node(prev).next;
            if pred(&self.node(cur).element) {
                return Some(self.unlink_after(prev));
            }
            prev = cur;
        }
        None
    }

    /// Drop every element for which `keep` is false, preserving the order and
    /// the current front of the rest.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let Some(t) = self.tail else { return };
        let mut prev = t;
        let mut remaining = self.size;
        while remaining > 0 {
            remaining -= 1;
            let cur = self.node(prev).next;
            if keep(&self.node(cur).element) {
                prev = cur;
            } else {
                self.unlink_after(prev);
                if self.tail.is_none() {
                    return;
                }
            }
        }
    }

    /// Front to back, once around.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let mut cur = self.tail.map(|t| self.node(t).next);
        let mut left = self.size;
        std::iter::from_fn(move || {
            if left == 0 {
                return None;
            }
            left -= 1;
            let n = self.node(cur?);
            cur = Some(n.next);
            Some(&n.element)
        })
    }

    fn unlink_after(&mut self, prev: usize) -> T {
        let cur = self.node(prev).next;
        if cur == prev {
            // The only node.
            self.tail = None;
        } else {
            let next = self.node(cur).next;
            self.node_mut(prev).next = next;
            if self.tail == Some(cur) {
                self.tail = Some(prev);
            }
        }
        self.size -= 1;
        self.free.push(cur);
        self.slots[cur].take().expect("linked slot is occupied").element
    }

    fn node(&self, i: usize) -> &Node<T> {
        self.slots[i].as_ref().expect("linked slot is occupied")
    }

    fn node_mut(&mut self, i: usize) -> &mut Node<T> {
        self.slots[i].as_mut().expect("linked slot is occupied")
    }

    fn alloc(&mut self, element: T) -> usize {
        let node = Node { element, next: 0 };
        match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(node);
                i
            }
            None => {
                self.slots.push(Some(node));
                self.slots.len() - 1
            }
        }
    }
}

impl<T> Default for CircularList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CircularList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Whether following `next` from `start` ever revisits a node.
///
/// Floyd's tortoise and hare: one marker steps once, the other twice. If the
/// chain ends, there is no cycle; if it loops, the fast marker laps the slow
/// one and they meet. Constant memory, which is the point: no visited set.
pub fn has_cycle<N: Copy + PartialEq>(start: N, next: impl Fn(N) -> Option<N>) -> bool {
    let mut slow = start;
    let mut fast = start;
    loop {
        let Some(f1) = next(fast) else { return false };
        let Some(f2) = next(f1) else { return false };
        fast = f2;
        slow = next(slow).expect("the slow marker trails the fast one");
        if slow == fast {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn items(c: &CircularList<i32>) -> Vec<i32> {
        c.iter().copied().collect()
    }

    #[test]
    fn turns_come_round_in_order_forever() {
        let mut c = CircularList::new();
        for i in 1..=3 {
            c.insert_last(i);
        }
        let turns: Vec<i32> = (0..7).map(|_| *c.turn().unwrap()).collect();
        assert_eq!(turns, vec![1, 2, 3, 1, 2, 3, 1]);
    }

    #[test]
    fn a_single_element_points_at_itself() {
        let mut c = CircularList::new();
        c.insert_last(7);
        assert_eq!(c.turn(), Some(&7));
        assert_eq!(c.turn(), Some(&7));
        assert_eq!(c.remove_where(|&x| x == 7), Some(7));
        assert!(c.is_empty());
        assert_eq!(c.turn(), None);
        assert_eq!(c.first(), None);
    }

    #[test]
    fn insert_goes_to_the_back_of_the_line() {
        let mut c = CircularList::new();
        c.insert_last(1);
        c.insert_last(2);
        c.turn(); // 1 has had its turn; 2 is next
        c.insert_last(3);
        assert_eq!(c.first(), Some(&2));
        assert_eq!(items(&c), vec![2, 1, 3]);
    }

    #[test]
    fn removing_the_tail_keeps_the_circle_closed() {
        let mut c = CircularList::new();
        for i in 1..=4 {
            c.insert_last(i);
        }
        assert_eq!(c.remove_where(|&x| x == 4), Some(4));
        assert_eq!(items(&c), vec![1, 2, 3]);
        assert_eq!(c.remove_where(|&x| x == 1), Some(1));
        assert_eq!(items(&c), vec![2, 3]);
        assert_eq!(c.remove_where(|&x| x == 9), None);
        let turns: Vec<i32> = (0..4).map(|_| *c.turn().unwrap()).collect();
        assert_eq!(turns, vec![2, 3, 2, 3]);
    }

    #[test]
    fn retain_keeps_order_and_reuses_slots() {
        let mut c = CircularList::new();
        for i in 1..=6 {
            c.insert_last(i);
        }
        c.retain(|x| x % 2 == 0);
        assert_eq!(items(&c), vec![2, 4, 6]);
        c.retain(|_| false);
        assert!(c.is_empty());
        for i in 0..6 {
            c.insert_last(i);
        }
        assert!(c.slots.len() <= 6);
    }

    #[test]
    fn floyd_finds_a_cycle_and_a_clean_chain() {
        // 5 -> 4 -> 3 -> 1 (init), and a cycle 10 -> 11 -> 12 -> 10.
        let parents: HashMap<i32, i32> =
            HashMap::from([(5, 4), (4, 3), (3, 1), (10, 11), (11, 12), (12, 10), (20, 10)]);
        let next = |p: i32| parents.get(&p).copied();
        assert!(!has_cycle(5, next));
        assert!(has_cycle(10, next));
        assert!(has_cycle(20, next), "a tail leading into a cycle");
        assert!(!has_cycle(99, next));
    }
}
