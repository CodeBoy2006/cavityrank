//! A packed four-slot Cuckoo Filter with implicit two- and four-level routing.
//!
//! [`Policy::CavityBit`], [`Policy::CavityRank4`], [`Policy::CavityRank4Path`],
//! and prepared [`Policy::DenseCavityRank4`] filters store routing ranks in
//! slot-pair orientations. Dense filters use Rotor before preparation. The
//! payload remains one `u64` per bucket, and lookups never decode routing or
//! path state.

mod bucket;
pub mod experiment;
mod filter;
pub mod oracle;

pub use filter::{
    Config, ConfigError, CuckooFilter, DensePreparationStats, InsertStats, PathActivation,
    PathConfig, PathReset, Policy,
};
