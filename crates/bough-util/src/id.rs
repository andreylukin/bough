//! Invariant: an opaque cross-boundary identifier is a branded newtype, never a bare `String`
//! (§0.2). `brand_id!` is the one way such a type is declared, so every id in the tree has the
//! same derives, the same serde shape and the same *absence* of `From<String>`.

/// Declares a newtype over `std::sync::Arc<str>`.
///
/// The generated type has `Debug`, `Display`, `Clone`, `PartialEq`, `Eq`, `Hash`, `PartialOrd`,
/// `Ord`, `serde::Serialize`, `serde::Deserialize`, `std::str::FromStr` and
/// `fn as_str(&self) -> &str`.
///
/// There is deliberately **no** `From<String>` / `From<&str>` impl: construction is
/// `Name::new(s)`, so a bare string can never become an id by inference. That absence is the
/// point of the macro, and it is checked by the fact that
///
/// ```compile_fail
/// bough_util::brand_id!(pub struct Demo;);
/// let _: Demo = "not an id".to_string().into();
/// ```
///
/// does not compile.
///
/// ```
/// bough_util::brand_id!(
///     /// An entry id, as it appears in a bundle row.
///     pub struct EntryId;
/// );
/// let id = EntryId::new("hello.greeter");
/// assert_eq!(id.as_str(), "hello.greeter");
/// ```
#[macro_export]
macro_rules! brand_id {
    ($(#[$m:meta])* $vis:vis struct $name:ident;) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        $vis struct $name(::std::sync::Arc<str>);

        impl $name {
            /// The only constructor. Takes anything string-shaped, on purpose explicitly.
            pub fn new(s: impl AsRef<str>) -> Self {
                Self(::std::sync::Arc::from(s.as_ref()))
            }
            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&*self.0, f)
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}({:?})", stringify!($name), &*self.0)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::std::convert::Infallible;
            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                ::std::result::Result::Ok(Self::new(s))
            }
        }

        impl ::std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}
