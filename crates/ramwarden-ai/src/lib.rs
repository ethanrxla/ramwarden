//! RamWarden's model tier.
//!
//! Four jobs, in descending order of how much they matter:
//!
//! 1. [`rerank`] ranks tabs against the user's stated goal by embedding
//!    similarity. Cheap, deterministic, and it replaces a hardcoded list of
//!    "time-wasting" domains with something that respects what the user is
//!    actually doing.
//! 2. [`chat`] asks a Nemotron model which applications and tabs to reclaim,
//!    constrained to a JSON schema.
//! 3. [`jev`] asks Jev, TypeSafe's System One model, whether the coldest apps
//!    on the LRU are safe to suspend or close before the ladder does it.
//! 4. [`vram`] accounts for GPU memory, so RamWarden manages the card as well as
//!    system RAM — and so it can tell when the GPU is too busy to ask.
//!
//! Every tier degrades: no cloud key falls back to the local model, no local
//! model falls back to the heuristic, and the heuristic always works.

pub mod client;
pub mod jev;
pub mod prompt;
pub mod provider;
pub mod rerank;
pub mod secret;
pub mod vram;
