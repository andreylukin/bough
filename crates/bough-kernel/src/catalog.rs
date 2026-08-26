//! Invariant: the catalog is compile-time (§0.4). A plugin is in the binary because a crate was
//! linked and submitted one `PluginRegistration` through `inventory`; there is no dynamic
//! registration path and no last-wins on a duplicate name — a duplicate is a hard error, because a
//! silent last-wins would make which plugin ran depend on link order.

use std::collections::BTreeMap;

use crate::plugin::ErasedPlugin;

/// One line in the compile-time catalog.
pub struct PluginRegistration {
    pub name: &'static str,
    pub ctor: fn() -> Box<dyn ErasedPlugin>,
}

inventory::collect!(PluginRegistration);

/// Register a `Plugin` impl with the compile-time catalog.
///
/// One line at the bottom of a plugin crate's `lib.rs`:
///
/// ```ignore
/// bough_kernel::register_plugin!(HelloPlugin);
/// ```
#[macro_export]
macro_rules! register_plugin {
    ($t:ty) => {
        $crate::inventory::submit! {
            $crate::catalog::PluginRegistration {
                name: <$t as $crate::plugin::Plugin>::NAME,
                ctor: || ::std::boxed::Box::new($crate::plugin::Shim::<$t>::new()),
            }
        }
    };
}

/// Name → plugin, built once at boot.
pub struct Catalog {
    plugins: BTreeMap<&'static str, Box<dyn ErasedPlugin>>,
}

impl Catalog {
    /// Every `register_plugin!` in the linked binary.
    ///
    /// `Err` on a duplicate name: two crates claiming one catalog name is a build-time bug that
    /// must not become a silent last-wins.
    pub fn from_inventory() -> Result<Catalog, CatalogError> {
        todo!("WP-3")
    }
    /// Look a plugin up by its catalog name.
    pub fn get(&self, name: &str) -> Option<&dyn ErasedPlugin> {
        self.plugins.get(name).map(|p| &**p)
    }
    /// Every registered name, sorted.
    pub fn names(&self) -> Vec<&'static str> {
        self.plugins.keys().copied().collect()
    }
    /// Test-only: a catalog built from an explicit list, so a unit test never sees the whole
    /// binary's registrations.
    pub fn from_parts(parts: Vec<PluginRegistration>) -> Result<Catalog, CatalogError> {
        todo!("WP-3")
    }
}

/// Failures building a catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("two plugins claim the catalog name `{0}`")]
    DuplicateName(&'static str),
}
