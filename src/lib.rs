mod node;
mod view;

pub use node::{build_tree, ConfigNode, NodeKind};
pub use view::{init, SchemaForm};

pub use schemars::schema_for;

/// Trait for controlling which nodes are visible and editable in the form.
///
/// Implement this trait to conditionally hide or disable config options based
/// on application state. Paths are dot-separated (e.g. `"server.hostname"`).
pub trait NodeFilter {
    /// Whether the node at `path` should be shown in the form.
    /// Hidden nodes and their children are completely omitted.
    fn visible(&self, _path: &str) -> bool {
        true
    }

    /// Whether the node at `path` can be edited.
    /// Disabled nodes are shown but rendered dimmed and ignore input.
    fn enabled(&self, _path: &str) -> bool {
        true
    }
}
