macro_rules! named_attrs_config {
    (
        $mod:ident::$struct:ident =>
        $( @$elided_name:ident = $elided_attr:ty ),* $(,)?
    ) => {
        named_attrs_config!{
            $mod::$struct =>
            merged:
            $( $elided_name = $elided_attr ),*
            ;
            $( $elided_name = $elided_attr ),*
        }
    };
    (
        $mod:ident::$struct:ident =>
        $( @$elided_name:ident = $elided_attr:ty, )*
        $( $name:ident = $attr:ty ),+ $(,)?
    ) => {
        named_attrs_config!{
            $mod::$struct =>
            merged:
            $( $elided_name = $elided_attr ),*
            ;
            $( $elided_name = $elided_attr, )*
            $( $name = $attr ),+
        }
    };
    (
        $mod:ident::$struct:ident =>
        merged:
        $( $elided_name:ident = $elided_attr:ty ),*
        ;
        $( $name:ident = $attr:ty ),*
    ) => {

        pub struct $struct {
            $(
                pub $name: Option<$attr>
            ),*
        }
        impl $struct {
            pub const fn empty() -> Self {
                Self {
                    $(
                        $name: None
                    ),*
                }
            }
        }

        mod $mod {
            use super::*;
            use proc_macro2::Span;
            use syn::{
                Error, Path, Token,
                parse::{Parse, discouraged::Speculative},
            };
            impl $struct {
                fn set_elided(&mut self, attr: $mod::ElidedAttr) {
                    match attr {
                        $(
                            $mod::ElidedAttr::$elided_name(attr) => {
                                assert!(self.$elided_name.is_none());
                                self.$elided_name = Some(attr);
                            }
                        )*
                    }
                }

                fn set_named(&mut self, attr: $mod::NamedAttr, span: Span) -> syn::Result<()> {
                    match attr {
                        $(
                            $mod::NamedAttr::$name(attr) => {
                                if self.$name.is_some() {
                                    return Err(Error::new(span, "duplicated config setting"));
                                }
                                self.$name = Some(attr);
                            }
                        )*
                    }
                    Ok(())
                }
            }

            impl Parse for $struct {
                fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
                    let mut config = Self::empty();
                    if input.is_empty() {
                        return Ok(config);
                    }
                    let full_order = ElidedOrder::orders();
                    let mut index = 0;
                    let mut current = full_order.get(index);
                    loop {
                        let span = input.span();
                        if let Some(&order) = current {
                            let fork = input.fork();
                            if let Some(attr) = NamedAttr::parse::<false>(input)? {
                                config.set_named(attr, span)?;
                                current = None;
                            } else {
                                let attr = order.parse(&fork)?;
                                config.set_elided(attr);
                                index += 1;
                                current = full_order.get(index);
                                input.advance_to(&fork);
                            }
                        } else {
                            let attr = NamedAttr::parse::<true>(input)?.unwrap();
                            config.set_named(attr, span)?;
                        };
                        if input.is_empty() {
                            return Ok(config);
                        }
                        input.parse::<Token![,]>()?;
                        if input.is_empty() {
                            return Ok(config);
                        }
                    }
                }
            }


            #[allow(non_camel_case_types)]
            enum ElidedAttr {
                $(
                    $elided_name($elided_attr)
                ),*
            }

            #[allow(non_camel_case_types)]
            #[derive(Clone, Copy)]
            enum ElidedOrder {
                $(
                    $elided_name
                ),*
            }

            impl ElidedOrder {
                const fn orders() -> &'static [Self] {
                    &[$(Self::$elided_name),*]
                }

                fn parse(self, input: syn::parse::ParseStream) -> syn::Result<ElidedAttr> {
                    match self {
                        $(
                            Self::$elided_name => Ok(ElidedAttr::$elided_name(input.parse()?)),
                        )*
                    }
                }
            }

            #[allow(non_camel_case_types)]
            enum NamedAttr {
                $(
                    $name($attr)
                ),*
            }

            impl NamedAttr {
                fn parse<const FORCE: bool>(input: syn::parse::ParseStream) -> syn::Result<Option<Self>> {
                    let path: Path = input.parse()?;
                    if input.peek(Token![=]) {
                        input.parse::<Token![=]>()?;
                        $(
                            if path.is_ident(stringify!($name)) {
                                return Ok(Some(NamedAttr::$name(input.parse()?)));
                            }
                        )*
                    }
                    if FORCE {
                        return Err(Error::new_spanned(
                            path,
                            format!(
                                "expected one of named settings {:?}",
                                [$( stringify!($name) ),*]
                            ),
                        ));
                    }
                    return Ok(None);
                }
            }
        }

    };
}
