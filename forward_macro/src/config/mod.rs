#[macro_use]
mod macros;

use syn::{
    Error, Expr, LitInt, LitStr, Path, parenthesized, parse::Parse, spanned::Spanned, token,
};

/// Path(Expr)
pub struct PathSelector {
    pub path: Path,
    pub selector: Option<Expr>,
}

impl Parse for PathSelector {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let path: Path = input.parse()?;
        let mut selector = None;
        if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            if !content.is_empty() {
                selector = Some(content.parse().map_err(|mut e| {
                    e.combine(Error::new(path.span(), "unexpected selector format"));
                    e
                })?);
            }
        }
        Ok(PathSelector { path, selector })
    }
}

named_attrs_config! {
    proxy_config::ProxyConfig =>
    @name = LitStr,
}

named_attrs_config! {
    forward_config::ForwardConfig =>
    @server = PathSelector,
    @thread = PathSelector,
    name = LitStr,
    timeout = LitInt,
}
