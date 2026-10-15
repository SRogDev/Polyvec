/// Vendoring patch (Polyvec): the upstream global allocator is removed.
/// Upstream sets mimalloc as the global allocator for its CLI/napi targets;
/// as a Polyvec library dependency that would silently replace the allocator
/// of every binary linking markit (including `polyvec-server`). Polyvec keeps
/// the system allocator; see VENDOR.md.

pub mod commands;
pub mod converters;
pub mod discover_markdown_source;
pub mod markit;
pub mod types;
pub mod utils;

#[cfg(feature = "napi")]
mod bindings;
