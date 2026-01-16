use std::fmt::Debug;

use proc_macro::TokenStream;

use quote::{ToTokens, quote};
use syn::{
    Expr, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, Pat, PatIdent, PatTuple, Token, TypeTuple,
    parse_quote, punctuated::Punctuated, spanned::Spanned,
};
#[macro_use]
mod config;
mod parser;

use config::*;

/// 宏定义，用于转发代理tarit实现
/// 提取`impl Trait for Target`中的所有`async`方法，然后创建一个代理结构体，
/// 代理结构体将重新实现`Trait`的所有`async`方法，调用时会转发到代理目标的`Target`对象，
/// 转发过程可能涉及远程过程调用或者本地调度器调用，这由转发协议在运行时决定。
///
/// # 代理使用样例
/// ```rust,ignore
/// use forward_macro::forward_proxy;
///
/// struct Target;
///
/// trait TargetTrait {
///
///     async fn method_self_args(&self, arg1: i32, arg2: String);
///
///     async fn method_self_args_return(&self, arg1: i32, arg2: String) -> String;
///
///     async fn method_args_return(arg1: &str) -> String;
/// }
///
/// #[forward_proxy(name = "TargetTrait")] // name可以设置代理trait的唯一标识符，默认为类型名
/// impl TargetTrait for Target {
///     /// 简化设置，固定转发至center服务器，默认线程，无别名，默认超时配置
///     #[forward(Center)]
///     async fn method_self_args(&self, arg1: i32, arg2: String) {
///         println!("method_with_self_and_args: {}, {}", arg1, arg2);
///     }
///
///     /// 指定转发服务器（Game）和执行线程（UserThread），设置通信协议内的方法别名
///     #[forward(server=Game, thread=UserThread(2), name="brief_fn"，timeout=None)]
///     async fn method_self_args_return(&self, sid: i32, uid: usize) -> String {
///         format!("method_with_self_and_args_and_return: {}, {}", sid, arg2)
///     }
///
///     /// 本地线程转发
///     #[forward(thread = EventThread)]
///     async fn method_args_return(arg1: &str) -> String {
///         format!("method_without_self: {}", arg1)
///     }
/// }
///
#[proc_macro_attribute]
pub fn forward_proxy(attrs: TokenStream, input: TokenStream) -> TokenStream {
    match parse(attrs, input) {
        Ok(ts) => {
            export_to_file("test", &ts);
            ts
        }
        Err(err) => err.into_compile_error().into(),
    }
}
fn parse(attrs: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let proxy_config = parser::parse_proxy_config(attrs)?;
    let mut implfor: ItemImpl = syn::parse(input)?;
    let proxy_name = match proxy_config.name {
        Some(name) => name,
        None => get_default_name(&implfor)?,
    };
    let mut proxyimpl = ItemImpl {
        attrs: implfor.attrs.clone(),
        defaultness: implfor.defaultness.clone(),
        unsafety: implfor.unsafety.clone(),
        impl_token: implfor.impl_token.clone(),
        generics: implfor.generics.clone(),
        trait_: implfor.trait_.clone(),
        self_ty: implfor.self_ty.clone(),
        brace_token: implfor.brace_token.clone(),
        items: vec![],
    };
    let target_type = proxyimpl.self_ty;
    proxyimpl.self_ty = parse_quote! { ::forward::ForwardProxy<#target_type> };
    for item in implfor.items.iter_mut() {
        if let ImplItem::Fn(implfn) = item {
            let config = parser::parse_forward_config(&mut implfn.attrs)?;
            let gen_server = match config.server {
                Some(PathSelector { path, selector }) => match selector {
                    Some(v) => quote!(::forward::ForwardSelector {
                        kind: #path,
                        selector: #v
                    }),
                    None => quote!(::forward::ForwardSelector {
                        kind: #path,
                        selector: ()
                    }),
                },
                None => quote!(::forward::ForwardSelector {
                    kind: (),
                    selector: ()
                }),
            };

            let gen_thread = match config.thread {
                Some(PathSelector { path, selector }) => match selector {
                    Some(v) => quote!(::forward::ForwardSelector {
                        kind: #path,
                        selector: #v
                    }),
                    None => quote!(::forward::ForwardSelector {
                        kind: #path,
                        selector: ()
                    }),
                },
                None => quote!(::forward::ForwardSelector {
                    kind: (),
                    selector: ()
                }),
            };
            let method_name = &implfn.sig.ident;
            let forward_name = match config.name {
                Some(name) => name,
                None => LitStr::new(&implfn.sig.ident.to_string(), implfn.sig.ident.span()),
            };

            let method_struct: Ident = if implfn.sig.asyncness.is_some() {
                parse_quote!(ForwardAsyncMethod)
            } else {
                parse_quote!(ForwardSyncMethod)
            };

            let method_call: Expr = if implfn.sig.asyncness.is_some() {
                parse_quote!(method.call_rpc(args).await.into_async().await)
            } else {
                parse_quote!(method.call_rpc(args).into_sync())
            };
            // 把方法中的参数类型列表打包为元组类型，作为原始闭包的单个参数类型
            let mut closure_args = Punctuated::<_, Token![,]>::new();
            implfn
                .sig
                .inputs
                .iter()
                .map(|arg| match arg {
                    syn::FnArg::Receiver(receiver) => receiver.ty.as_ref().clone(),
                    syn::FnArg::Typed(pat_type) => pat_type.ty.as_ref().clone(),
                })
                .for_each(|ty| {
                    closure_args.push(ty);
                });
            let closure_args = TypeTuple {
                paren_token: Default::default(),
                elems: closure_args,
            };
            // 把方法中的参数列表转换为元组匹配模式，用于拆解闭包中的元组参数
            let mut destruct_args = Punctuated::new();
            implfn
                .sig
                .inputs
                .iter()
                .map(|arg| match arg {
                    syn::FnArg::Receiver(receiver) => Pat::Ident(PatIdent {
                        attrs: vec![],
                        by_ref: None,
                        mutability: receiver.mutability,
                        ident: Ident::new("target", receiver.span()),
                        subpat: None,
                    }),
                    syn::FnArg::Typed(pt) => pt.pat.as_ref().clone(),
                })
                .for_each(|pat| {
                    destruct_args.push(pat);
                });
            let destruct_args = PatTuple {
                attrs: vec![],
                paren_token: Default::default(),
                elems: destruct_args,
            };
            let mut struct_args = Punctuated::new();
            implfn
                .sig
                .inputs
                .iter()
                .map(|arg| match arg {
                    syn::FnArg::Receiver(receiver) => Pat::Ident(PatIdent {
                        attrs: vec![],
                        by_ref: None,
                        mutability: receiver.mutability,
                        ident: Ident::new("self", receiver.span()),
                        subpat: None,
                    }),
                    syn::FnArg::Typed(pt) => pt.pat.as_ref().clone(),
                })
                .for_each(|pat| {
                    struct_args.push(pat);
                });
            let struct_args = PatTuple {
                attrs: vec![],
                paren_token: Default::default(),
                elems: struct_args,
            };
            let body = parse_quote! {
                {
                    let config = ::forward::ForwardConfig {
                        server: #gen_server,
                        thread: #gen_thread,
                    };
                    let meta = ::forward::ForwardMetadata::new(#proxy_name, #forward_name);
                    let method = ::forward::#method_struct::new(config, meta, |args: #closure_args| {
                        let #destruct_args = args;
                        target.as_raw_ref().#method_name()
                    });
                    let args = #struct_args;
                    #method_call
                }
            };
            let proxyfn = ImplItemFn {
                attrs: implfn.attrs.clone(),
                vis: implfn.vis.clone(),
                defaultness: implfn.defaultness.clone(),
                sig: implfn.sig.clone(),
                block: body,
            };
            proxyimpl.items.push(ImplItem::Fn(proxyfn));
        } else {
            proxyimpl.items.push(item.clone());
        }
    }

    Ok(quote! {
        #implfor

        #proxyimpl
    }
    .into_token_stream()
    .into())
}

fn get_default_name(implfor: &ItemImpl) -> syn::Result<LitStr> {
    let Some((_, ref path, _)) = implfor.trait_ else {
        return Err(syn::Error::new_spanned(implfor, "missing proxy name"));
    };
    let name = path.segments.last().map(|p| &p.ident);
    if let Some(name) = name {
        Ok(LitStr::new(&name.to_string(), name.span()))
    } else {
        Err(syn::Error::new_spanned(path, "missing proxy name"))
    }
}

fn export_to_file(file_postfix: &str, output: &TokenStream) -> bool {
    use std::io::Write;

    if let Ok(var) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut path = std::path::PathBuf::from(var);
        loop {
            {
                let mut path = path.clone();
                path.push("target");
                if path.exists() {
                    path.push("generated");
                    path.push("forward");
                    if std::fs::create_dir_all(&path).is_err() {
                        return false;
                    }
                    path.push(format!("forward_proxy_{}.rs", file_postfix));
                    if let Ok(mut file) = std::fs::File::create(path) {
                        let _ = file.write_all(output.to_string().as_bytes());
                        return true;
                    }
                }
            }
            if let Some(parent) = path.parent() {
                path = parent.into();
            } else {
                break;
            }
        }
    }
    false
}

impl Debug for ForwardConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardConfig")
            .field(
                "server",
                &self.server.as_ref().map(|v| {
                    if let Some(ref s) = v.selector {
                        format!(
                            "{}({})",
                            v.path.to_token_stream().to_string(),
                            s.to_token_stream().to_string()
                        )
                    } else {
                        format!("{}", v.path.to_token_stream().to_string())
                    }
                }),
            )
            .field(
                "thread",
                &self.thread.as_ref().map(|v| {
                    if let Some(ref s) = v.selector {
                        format!(
                            "{}({})",
                            v.path.to_token_stream().to_string(),
                            s.to_token_stream().to_string()
                        )
                    } else {
                        format!("{}", v.path.to_token_stream().to_string())
                    }
                }),
            )
            .field(
                "name",
                &self.name.as_ref().map(|v| v.to_token_stream().to_string()),
            )
            .field(
                "timeout",
                &self
                    .timeout
                    .as_ref()
                    .map(|v| v.to_token_stream().to_string()),
            )
            .finish()
    }
}

impl Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("name", &self.name.as_ref().map(|v| v.value()))
            .finish()
    }
}
