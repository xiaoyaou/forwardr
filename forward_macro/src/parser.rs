use proc_macro::TokenStream;
use syn::{Attribute, Error, Meta};

use crate::config::*;

pub fn parse_proxy_config(attrs: TokenStream) -> syn::Result<ProxyConfig> {
    syn::parse(attrs)
}

pub fn parse_forward_config(attrs: &mut Vec<Attribute>) -> syn::Result<ForwardConfig> {
    let Some(index) = attrs
        .iter()
        .position(|attr| attr.path().is_ident("forward"))
    else {
        return Ok(ForwardConfig::empty());
    };
    let forward = attrs.remove(index);
    match forward.meta {
        Meta::Path(_) => {
            return Ok(ForwardConfig::empty());
        }
        Meta::NameValue(nv) => {
            return Err(Error::new_spanned(
                nv,
                "expected parentheses #[forward(...)]",
            ));
        }
        Meta::List(meta) => meta.parse_args(),
    }
}
