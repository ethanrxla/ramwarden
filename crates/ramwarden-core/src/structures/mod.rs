//! The linked structures RamWarden's policy is built on.
//!
//! | Structure | Shape | Used for |
//! |---|---|---|
//! | [`Lru`] | hash map + doubly linked list | which apps the user touched least recently |
//! | [`LinkedQueue`] | singly linked, head and tail | kills waiting out their grace period, in order |
//! | [`LinkedStack`] | singly linked, head only | reversible actions, so the newest is undone first |
//! | [`CircularList`] | circularly linked, tail only | taking turns at page-out, so every idle process gets one |
//!
//! [`circular::has_cycle`] is Floyd's tortoise and hare, used on the process
//! tree so a malformed parent chain cannot spin the detector.
//!
//! The stack and queue follow the classic textbook layout (a nested node type,
//! `head`, `size`, `push`/`pop`/`top` and `enqueue`/`dequeue`/`first`), with
//! `Option` in place of an `Empty` exception.

pub mod circular;
pub mod lru;
pub mod queue;
pub mod stack;

pub use circular::CircularList;
pub use lru::Lru;
pub use queue::LinkedQueue;
pub use stack::LinkedStack;
