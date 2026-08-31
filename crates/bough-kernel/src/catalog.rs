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
        Self::build(inventory::iter::<PluginRegistration>)
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
        Self::build(parts.iter())
    }

    fn build<'a>(
        regs: impl IntoIterator<Item = &'a PluginRegistration>,
    ) -> Result<Catalog, CatalogError> {
        let mut plugins: BTreeMap<&'static str, Box<dyn ErasedPlugin>> = BTreeMap::new();
        for reg in regs {
            if plugins.contains_key(reg.name) {
                return Err(CatalogError::DuplicateName(reg.name));
            }
            plugins.insert(reg.name, (reg.ctor)());
        }
        Ok(Catalog { plugins })
    }
}

/// Failures building a catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("two plugins claim the catalog name `{0}`")]
    DuplicateName(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::error::PluginError;
    use crate::plugin::{Plugin, Shim};
    use std::sync::Arc;

    /// A plugin that exists only to be found in the catalog. `apply` is never called from these
    /// tests: the catalog builds shims, it does not mount them.
    pub struct CatalogProbe;

    #[derive(
        Default, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
    )]
    pub struct ProbeConfig {
        #[serde(default)]
        pub note: String,
    }

    #[async_trait::async_trait]
    impl Plugin for CatalogProbe {
        const NAME: &'static str = "catalog-probe";
        type Config = ProbeConfig;
        async fn apply(_ctx: Context, _cfg: Arc<ProbeConfig>) -> Result<(), PluginError> {
            unreachable!("the catalog never mounts")
        }
    }

    crate::register_plugin!(CatalogProbe);

    fn reg(name: &'static str) -> PluginRegistration {
        PluginRegistration {
            name,
            ctor: || Box::new(Shim::<CatalogProbe>::new()),
        }
    }

    #[test]
    fn inventory_finds_registered_plugins() {
        let c = Catalog::from_inventory().expect("no duplicate names in the test binary");
        assert!(
            c.names().contains(&"catalog-probe"),
            "register_plugin! did not reach the catalog: {:?}",
            c.names()
        );
        assert_eq!(
            c.get("catalog-probe").map(|p| p.name()),
            Some("catalog-probe")
        );
    }

    #[test]
    fn duplicate_catalog_name_is_an_error() {
        let err = match Catalog::from_parts(vec![reg("dup"), reg("dup")]) {
            Ok(_) => panic!("a duplicate catalog name must be a hard error, not last-wins"),
            Err(e) => e,
        };
        match err {
            CatalogError::DuplicateName(n) => assert_eq!(n, "dup"),
        }
    }

    #[test]
    fn from_parts_builds_an_isolated_catalog() {
        let c = Catalog::from_parts(vec![reg("only-me")]).unwrap();
        assert_eq!(c.names(), vec!["only-me"]);
        assert!(
            c.get("catalog-probe").is_none(),
            "from_parts must not see the whole binary"
        );
    }
}
