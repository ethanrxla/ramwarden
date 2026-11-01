//! A LIFO stack stored in a singly linked list.
//!
//! The top of the stack is the head of the list, so `push`, `pop` and `top` are
//! all O(1) and never reallocate.

/// A singly linked node. Not public: callers see elements, never links.
struct Node<T> {
    element: T,
    next: Option<Box<Node<T>>>,
}

pub struct LinkedStack<T> {
    head: Option<Box<Node<T>>>,
    size: usize,
}

impl<T> LinkedStack<T> {
    /// Create an empty stack.
    pub fn new() -> Self {
        LinkedStack { head: None, size: 0 }
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Add `e` to the top of the stack.
    pub fn push(&mut self, e: T) {
        let next = self.head.take();
        self.head = Some(Box::new(Node { element: e, next }));
        self.size += 1;
    }

    /// The element at the top, without removing it. `None` when empty.
    pub fn top(&self) -> Option<&T> {
        self.head.as_ref().map(|n| &n.element)
    }

    /// Remove and return the element at the top. `None` when empty.
    pub fn pop(&mut self) -> Option<T> {
        self.head.take().map(|node| {
            self.head = node.next; // bypass the former top node
            self.size -= 1;
            node.element
        })
    }

    /// Drop every element.
    pub fn clear(&mut self) {
        while self.pop().is_some() {}
    }

    /// Top to bottom.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let mut cur = self.head.as_deref();
        std::iter::from_fn(move || {
            let node = cur?;
            cur = node.next.as_deref();
            Some(&node.element)
        })
    }
}

impl<T> Default for LinkedStack<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Unlink iteratively. The derived drop recurses once per node, and a long
/// enough stack would overflow the thread's own stack doing it.
impl<T> Drop for LinkedStack<T> {
    fn drop(&mut self) {
        let mut cur = self.head.take();
        while let Some(mut node) = cur {
            cur = node.next.take();
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for LinkedStack<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_in_first_out() {
        let mut s = LinkedStack::new();
        assert!(s.is_empty());
        s.push(5);
        s.push(3);
        s.push(9);
        assert_eq!(s.len(), 3);
        assert_eq!(s.pop(), Some(9));
        assert_eq!(s.top(), Some(&3));
        assert_eq!(s.iter().copied().collect::<Vec<_>>(), vec![3, 5]);
        assert_eq!(s.pop(), Some(3));
        assert_eq!(s.pop(), Some(5));
        assert_eq!(s.pop(), None);
        assert!(s.is_empty());
    }

    #[test]
    fn a_long_stack_drops_without_recursing() {
        let mut s = LinkedStack::new();
        for i in 0..1_000_000 {
            s.push(i);
        }
        drop(s);
    }
}
