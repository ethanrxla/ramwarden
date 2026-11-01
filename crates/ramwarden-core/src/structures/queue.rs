//! A FIFO queue stored in a singly linked list.
//!
//! Elements leave from the head and join at the tail. Keeping a tail pointer as
//! well as the head is what makes `enqueue` O(1): without it, every join would
//! walk the whole list to find the end.

use std::ptr;

struct Node<T> {
    element: T,
    next: Option<Box<Node<T>>>,
}

pub struct LinkedQueue<T> {
    head: Option<Box<Node<T>>>,
    /// The last node, or null when empty. It aliases a node owned through
    /// `head`, so it is only ever dereferenced while `head` keeps that node
    /// alive, and reset whenever the list empties.
    tail: *mut Node<T>,
    size: usize,
}

// SAFETY: `tail` only ever points into nodes this queue owns, so sending the
// queue sends everything it points at along with it. It is never shared.
unsafe impl<T: Send> Send for LinkedQueue<T> {}
// SAFETY: `&LinkedQueue` gives no way to reach `tail`; only `&mut` methods use it.
unsafe impl<T: Sync> Sync for LinkedQueue<T> {}

impl<T> LinkedQueue<T> {
    /// Create an empty queue.
    pub fn new() -> Self {
        LinkedQueue {
            head: None,
            tail: ptr::null_mut(),
            size: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// The element at the front, without removing it. `None` when empty.
    pub fn first(&self) -> Option<&T> {
        self.head.as_ref().map(|n| &n.element)
    }

    /// Add `e` to the back of the queue.
    pub fn enqueue(&mut self, e: T) {
        let mut node = Box::new(Node {
            element: e,
            next: None,
        });
        let raw: *mut Node<T> = &mut *node;
        if self.tail.is_null() {
            self.head = Some(node);
        } else {
            // SAFETY: a non-null tail is the last node of the list owned by
            // `head`, and we hold `&mut self`, so nothing else aliases it.
            unsafe { (*self.tail).next = Some(node) };
        }
        self.tail = raw;
        self.size += 1;
    }

    /// Remove and return the element at the front. `None` when empty.
    pub fn dequeue(&mut self) -> Option<T> {
        self.head.take().map(|node| {
            self.head = node.next;
            if self.head.is_none() {
                self.tail = ptr::null_mut();
            }
            self.size -= 1;
            node.element
        })
    }

    /// Drop every element.
    pub fn clear(&mut self) {
        while self.dequeue().is_some() {}
    }

    /// Front to back.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let mut cur = self.head.as_deref();
        std::iter::from_fn(move || {
            let node = cur?;
            cur = node.next.as_deref();
            Some(&node.element)
        })
    }
}

impl<T> Default for LinkedQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for LinkedQueue<T> {
    fn drop(&mut self) {
        let mut cur = self.head.take();
        while let Some(mut node) = cur {
            cur = node.next.take();
        }
        self.tail = ptr::null_mut();
    }
}

impl<T: Clone> Clone for LinkedQueue<T> {
    fn clone(&self) -> Self {
        let mut q = LinkedQueue::new();
        for e in self.iter() {
            q.enqueue(e.clone());
        }
        q
    }
}

impl<T> FromIterator<T> for LinkedQueue<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut q = LinkedQueue::new();
        for e in iter {
            q.enqueue(e);
        }
        q
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for LinkedQueue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_in_first_out() {
        let mut q = LinkedQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.first(), None);
        q.enqueue("a");
        q.enqueue("b");
        q.enqueue("c");
        assert_eq!(q.len(), 3);
        assert_eq!(q.first(), Some(&"a"));
        assert_eq!(q.dequeue(), Some("a"));
        assert_eq!(q.dequeue(), Some("b"));
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec!["c"]);
        assert_eq!(q.dequeue(), Some("c"));
        assert_eq!(q.dequeue(), None);
        assert!(q.is_empty());
    }

    #[test]
    fn emptying_and_refilling_resets_the_tail() {
        let mut q = LinkedQueue::new();
        q.enqueue(1);
        assert_eq!(q.dequeue(), Some(1));
        // A stale tail here would write into a freed node.
        q.enqueue(2);
        q.enqueue(3);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn a_clone_is_independent() {
        let mut q: LinkedQueue<String> = ["x", "y"].iter().map(|s| s.to_string()).collect();
        let c = q.clone();
        q.dequeue();
        q.enqueue("z".into());
        assert_eq!(c.iter().cloned().collect::<Vec<_>>(), vec!["x", "y"]);
        assert_eq!(q.iter().cloned().collect::<Vec<_>>(), vec!["y", "z"]);
    }

    #[test]
    fn a_long_queue_drops_without_recursing() {
        let q: LinkedQueue<u32> = (0..1_000_000).collect();
        drop(q);
    }
}
