//! RamWarden's learned components. See [`jepa`] for the reuse predictor.

pub mod features;
pub mod jepa;

#[cfg(feature = "embed")]
pub mod embed;
