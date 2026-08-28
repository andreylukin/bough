//! Invariant (decision D-C7): the scan is LEXICAL and complete over the roots it is given. A regex
//! scanner would miss exactly the cases that matter — a `const MODE` override that disagrees with
//! its trait, one `NAME` under two modes, a type impl'ing two event traits dispatched under the
//! wrong one — so this parses with `syn` and a file that does not parse is an ERROR naming the
//! file, never a silently skipped file (§16).

use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// The four dispatch modes the kernel's event traits spell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchMode {
    Emit,
    Parallel,
    Serial,
    Waterfall,
}

impl DispatchMode {
    /// The mode an event trait's path names, when it is one of the four.
    pub fn from_trait(path: &str) -> Option<DispatchMode> {
        match last_segment(path) {
            "EmitEvent" => Some(DispatchMode::Emit),
            "ParallelEvent" => Some(DispatchMode::Parallel),
            "SerialEvent" => Some(DispatchMode::Serial),
            "WaterfallEvent" => Some(DispatchMode::Waterfall),
            _ => None,
        }
    }

    /// The mode a dispatch method name names.
    pub fn from_method(name: &str) -> Option<DispatchMode> {
        match name {
            "emit" => Some(DispatchMode::Emit),
            "parallel" => Some(DispatchMode::Parallel),
            "serial" => Some(DispatchMode::Serial),
            "waterfall" => Some(DispatchMode::Waterfall),
            _ => None,
        }
    }

    /// The mode a `DispatchMode::<Variant>` path names.
    fn from_variant(path: &str) -> Option<DispatchMode> {
        match last_segment(path) {
            "Emit" => Some(DispatchMode::Emit),
            "Parallel" => Some(DispatchMode::Parallel),
            "Serial" => Some(DispatchMode::Serial),
            "Waterfall" => Some(DispatchMode::Waterfall),
            _ => None,
        }
    }

    /// The word the table and the findings print.
    pub fn as_str(self) -> &'static str {
        match self {
            DispatchMode::Emit => "emit",
            DispatchMode::Parallel => "parallel",
            DispatchMode::Serial => "serial",
            DispatchMode::Waterfall => "waterfall",
        }
    }
}

/// The last `::`-separated segment of a path, with any generic arguments dropped.
fn last_segment(path: &str) -> &str {
    let head = path.split('<').next().unwrap_or(path);
    head.rsplit("::").next().unwrap_or(head).trim()
}

/// One declared event: an `impl <EventTrait> for <Ty>` with its `const NAME`.
#[derive(Clone, Debug, PartialEq)]
pub struct EventDecl {
    /// The `const NAME` literal.
    pub name: String,
    /// The impl's `Self` type.
    pub ty: String,
    /// From the TRAIT: Emit / Parallel / Serial / Waterfall.
    pub trait_mode: DispatchMode,
    /// An explicit `const MODE = …`, when the impl carries one.
    pub declared_mode: Option<DispatchMode>,
    pub krate: String,
    pub file: PathBuf,
    pub line: usize,
}

impl EventDecl {
    /// What the tree will actually dispatch this under: the override when there is one.
    pub fn effective_mode(&self) -> DispatchMode {
        self.declared_mode.unwrap_or(self.trait_mode)
    }
}

/// Whether a site DISPATCHES an event or LISTENS for one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SiteKind {
    Dispatch,
    Listen,
}

/// One call site: `ctx.emit::<X>(…)`, `ctx.waterfall::<X>(…)`, `ctx.on::<X>(…)`.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchSite {
    pub ty: String,
    pub mode: DispatchMode,
    pub kind: SiteKind,
    pub file: PathBuf,
    pub line: usize,
}

/// Everything one scan found.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub decls: Vec<EventDecl>,
    pub sites: Vec<DispatchSite>,
}

/// Why a scan could not finish.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} does not parse as Rust: {1}")]
    Parse(PathBuf, syn::Error),
}

/// Parse every `.rs` under `roots` with `syn` and collect declarations and sites.
pub fn scan(roots: &[&Path]) -> Result<Catalog, ScanError> {
    let mut files = Vec::new();
    for root in roots {
        collect_rs(root, &mut files)?;
    }
    files.sort();
    let mut catalog = Catalog {
        decls: Vec::new(),
        sites: Vec::new(),
    };
    for file in files {
        scan_file(&file, &mut catalog)?;
    }
    Ok(catalog)
}

/// Parse ONE file into `catalog`. The unit tests drive this; `scan` is the walk around it.
pub fn scan_file(file: &Path, catalog: &mut Catalog) -> Result<(), ScanError> {
    let src = std::fs::read_to_string(file).map_err(|e| ScanError::Io(file.to_path_buf(), e))?;
    scan_source(file, &src, catalog)
}

/// Parse one source STRING as if it lived at `file`. Pure; the tests use it directly.
pub fn scan_source(file: &Path, src: &str, catalog: &mut Catalog) -> Result<(), ScanError> {
    let ast = syn::parse_file(src).map_err(|e| ScanError::Parse(file.to_path_buf(), e))?;
    let mut v = Visitor {
        file: file.to_path_buf(),
        krate: krate_of(file),
        generics: Vec::new(),
        catalog,
    };
    v.visit_file(&ast);
    Ok(())
}

/// The crate a path belongs to: the directory under the root (`crates/bough-kernel/…`).
fn krate_of(file: &Path) -> String {
    let comps: Vec<String> = file
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (i, c) in comps.iter().enumerate() {
        if (c == "crates" || c == "plugins") && i + 1 < comps.len() {
            return comps[i + 1].clone();
        }
    }
    comps.first().cloned().unwrap_or_default()
}

/// Walk for `.rs` files. `target/`, dotted dirs and `fixtures/` are skipped: the planted fixtures
/// are deliberate mismatches and must never enter the tree's own catalog.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ScanError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => return Err(ScanError::Io(dir.to_path_buf(), e)),
    };
    for entry in rd {
        let entry = entry.map_err(|e| ScanError::Io(dir.to_path_buf(), e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name.starts_with('.') || name == "target" || name == "fixtures" {
                continue;
            }
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

struct Visitor<'a> {
    file: PathBuf,
    krate: String,
    /// Generic type parameters in scope. A turbofish naming one is a generic helper, not an event.
    generics: Vec<String>,
    catalog: &'a mut Catalog,
}

fn path_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn generic_idents(g: &syn::Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            syn::GenericParam::Type(t) => Some(t.ident.to_string()),
            _ => None,
        })
        .collect()
}

impl Visitor<'_> {
    fn with_generics<F: FnOnce(&mut Self)>(&mut self, g: &syn::Generics, f: F) {
        let added = generic_idents(g);
        let n = added.len();
        self.generics.extend(added);
        f(self);
        self.generics.truncate(self.generics.len() - n);
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if let Some((_, trait_path, _)) = &node.trait_ {
            let tp = path_string(trait_path);
            if let Some(trait_mode) = DispatchMode::from_trait(&tp) {
                let ty = type_name(&node.self_ty).unwrap_or_else(|| "<unknown>".into());
                let mut name = None;
                let mut declared_mode = None;
                for item in &node.items {
                    if let syn::ImplItem::Const(c) = item {
                        if c.ident == "NAME" {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(s),
                                ..
                            }) = &c.expr
                            {
                                name = Some(s.value());
                            }
                        } else if c.ident == "MODE" {
                            if let syn::Expr::Path(p) = &c.expr {
                                declared_mode = DispatchMode::from_variant(&path_string(&p.path));
                            }
                        }
                    }
                }
                if let Some(name) = name {
                    self.catalog.decls.push(EventDecl {
                        name,
                        ty,
                        trait_mode,
                        declared_mode,
                        krate: self.krate.clone(),
                        file: self.file.clone(),
                        line: node.impl_token.span.start().line,
                    });
                }
            }
        }
        let g = node.generics.clone();
        self.with_generics(&g, |s| syn::visit::visit_item_impl(s, node));
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let g = node.sig.generics.clone();
        self.with_generics(&g, |s| syn::visit::visit_item_fn(s, node));
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let g = node.sig.generics.clone();
        self.with_generics(&g, |s| syn::visit::visit_impl_item_fn(s, node));
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let g = node.sig.generics.clone();
        self.with_generics(&g, |s| syn::visit::visit_trait_item_fn(s, node));
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        let kind = match method.as_str() {
            "on" | "on_with" => Some(SiteKind::Listen),
            _ => DispatchMode::from_method(&method).map(|_| SiteKind::Dispatch),
        };
        if let (Some(kind), Some(turbofish)) = (kind, node.turbofish.as_ref()) {
            let first = turbofish.args.iter().find_map(|a| match a {
                syn::GenericArgument::Type(t) => type_name(t),
                _ => None,
            });
            if let Some(ty) = first {
                if !self.generics.contains(&ty) {
                    // A listener carries no mode of its own: the mode is the declaration's, and the
                    // ListenModeDiffers check is what a future typed `on` would give us for free.
                    let mode = DispatchMode::from_method(&method).unwrap_or(DispatchMode::Emit);
                    self.catalog.sites.push(DispatchSite {
                        ty,
                        mode,
                        kind,
                        file: self.file.clone(),
                        line: node.method.span().start().line,
                    });
                }
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_str(src: &str) -> Catalog {
        let mut c = Catalog {
            decls: Vec::new(),
            sites: Vec::new(),
        };
        scan_source(Path::new("crates/fixture/src/lib.rs"), src, &mut c).unwrap();
        c
    }

    #[test]
    fn an_impl_of_each_trait_is_found_with_its_mode() {
        let c = scan_str(
            r#"
            impl EmitEvent for A { const NAME: &'static str = "a"; }
            impl ParallelEvent for B { const NAME: &'static str = "b"; }
            impl SerialEvent for C { const NAME: &'static str = "c"; }
            impl bough_kernel::event::WaterfallEvent for D { const NAME: &'static str = "d"; }
            impl Display for E { const NAME: &'static str = "no"; }
            "#,
        );
        let got: Vec<_> = c
            .decls
            .iter()
            .map(|d| (d.name.as_str(), d.ty.as_str(), d.trait_mode))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", "A", DispatchMode::Emit),
                ("b", "B", DispatchMode::Parallel),
                ("c", "C", DispatchMode::Serial),
                ("d", "D", DispatchMode::Waterfall),
            ]
        );
        assert_eq!(c.decls[0].krate, "fixture");
    }

    #[test]
    fn a_const_mode_override_is_recorded_separately_from_the_trait() {
        let c = scan_str(
            r#"
            impl EmitEvent for A {
                const NAME: &'static str = "a";
                const MODE: DispatchMode = DispatchMode::Serial;
            }
            impl EmitEvent for B { const NAME: &'static str = "b"; }
            "#,
        );
        assert_eq!(c.decls[0].trait_mode, DispatchMode::Emit);
        assert_eq!(c.decls[0].declared_mode, Some(DispatchMode::Serial));
        assert_eq!(c.decls[0].effective_mode(), DispatchMode::Serial);
        assert_eq!(c.decls[1].declared_mode, None);
        assert_eq!(c.decls[1].effective_mode(), DispatchMode::Emit);
    }

    #[test]
    fn a_turbofish_dispatch_site_records_its_mode() {
        let c = scan_str(
            r#"
            fn f(ctx: &Context) {
                ctx.emit::<A>(());
                ctx.parallel::<crate::event::B>(());
                ctx.serial::<C>(());
                ctx.waterfall::<D>(());
                ctx.emit(());
                ctx.other::<E>(());
            }
            fn generic<X: EmitEvent>(ctx: &Context) { ctx.emit::<X>(()); }
            "#,
        );
        let got: Vec<_> = c
            .sites
            .iter()
            .map(|s| (s.ty.as_str(), s.mode, s.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                ("A", DispatchMode::Emit, SiteKind::Dispatch),
                ("B", DispatchMode::Parallel, SiteKind::Dispatch),
                ("C", DispatchMode::Serial, SiteKind::Dispatch),
                ("D", DispatchMode::Waterfall, SiteKind::Dispatch),
            ],
            "a bare call, a non-dispatch method and a generic parameter are all not sites"
        );
    }

    #[test]
    fn a_listener_registration_records_a_listen_site() {
        let c = scan_str(
            r#"
            fn f(ctx: &Context) {
                ctx.on::<A, _, _>(|_| async {});
                ctx.on_with::<B, _, _>(opts, |_| async {});
            }
            "#,
        );
        let got: Vec<_> = c.sites.iter().map(|s| (s.ty.as_str(), s.kind)).collect();
        assert_eq!(got, vec![("A", SiteKind::Listen), ("B", SiteKind::Listen)]);
    }

    #[test]
    fn a_file_that_does_not_parse_is_an_error_naming_the_file() {
        let mut c = Catalog {
            decls: Vec::new(),
            sites: Vec::new(),
        };
        let err = scan_source(Path::new("crates/x/src/broken.rs"), "fn ( {{{", &mut c).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("crates/x/src/broken.rs"), "{msg}");
        assert!(msg.contains("does not parse"), "{msg}");
    }
}
