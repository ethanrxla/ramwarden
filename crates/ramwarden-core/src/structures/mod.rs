//! The linked structures RamWarden's policy is built on.
//!
//! | Structure | Shape | Used for |
//! |---|---|---|
//! | [`Lru`] | hash map + doubly linked list | which apps the user touched least recently |
//! | [`LinkedQueue`] | singly linked, head and tail | kills waiting out their grace period, in order |
//! | [`LinkedStack`] | singly linked, head only | reversible actions, so the newest is undone first |
//!
//! The stack and queue follow the classic textbook layout (a nested node type,
//! `head`, `size`, `push`/`pop`/`top` and `enqueue`/`dequeue`/`first`), with
//! `Option` in place of an `Empty` exception.

pub mod lru;
pub mod queue;
pub mod stack;

pub use lru::Lru;
pub use queue::LinkedQueue;
pub use stack::LinkedStack;
