#![allow(dead_code)]
use std::future::Future;
use std::pin::Pin;

use forward::*;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[tokio::main]
async fn main() {
    let proxy = ForwardProxy::new(Target {
        something: "main".to_owned(),
    });

    let sync_value = proxy.get_remote_something();
    println!("sync: {}", sync_value);

    let async_value = proxy.async_get_remote().await;
    println!("async: {}", async_value);
}

#[derive(Clone, Copy, Default)]
struct Center;

#[derive(Clone, Copy, Default)]
struct Local;

#[derive(Clone, Copy, Default)]
struct UserThread;

#[derive(Clone, Copy, Default)]
struct RpcForward;

#[derive(Clone, Copy, Default)]
struct LocalForward;

impl ForwardKind for Center {
    type Index = ();
    type Handle = RpcForward;

    fn select(&self, _: Self::Index) -> Self::Handle {
        RpcForward
    }
}

impl ForwardKind for Local {
    type Index = ();
    type Handle = LocalForward;

    fn select(&self, _: Self::Index) -> Self::Handle {
        LocalForward
    }
}

impl ForwardKind for UserThread {
    type Index = u32;
    type Handle = LocalForward;

    fn select(&self, _: Self::Index) -> Self::Handle {
        LocalForward
    }
}

impl Forward for RpcForward {
    fn forward_sync<A, B, C, D, Args, R>(
        &self,
        method: ForwardSyncMethod<A, B, C, D, Args, R>,
        args: Args,
    ) -> ForwardReturn<R>
    where
        A: ForwardKind,
        B: Selector<Args, Output = A::Index>,
        C: ForwardKind,
        D: Selector<Args, Output = C::Index>,
    {
        let meta = method.metadata();
        println!("→ rpc hop :: {}::{}", meta.name(), meta.method());
        method.call_local(args)
    }

    async fn forward_async<A, B, C, D, Args, R, F>(
        &self,
        method: ForwardAsyncMethod<A, B, C, D, Args, R, F>,
        args: Args,
    ) -> ForwardReturn<R>
    where
        A: ForwardKind,
        B: Selector<Args, Output = A::Index>,
        C: ForwardKind,
        D: Selector<Args, Output = C::Index>,
        Args: Send,
        R: Send,
        F: Future<Output = R> + Send,
    {
        let meta = method.metadata();
        println!("→ rpc hop (async) :: {}::{}", meta.name(), meta.method());
        method.call_local(args).await
    }
}

impl Forward for LocalForward {
    fn forward_sync<A, B, C, D, Args, R>(
        &self,
        method: ForwardSyncMethod<A, B, C, D, Args, R>,
        args: Args,
    ) -> ForwardReturn<R>
    where
        A: ForwardKind,
        B: Selector<Args, Output = A::Index>,
        C: ForwardKind,
        D: Selector<Args, Output = C::Index>,
    {
        let meta = method.metadata();
        println!("→ local hop :: {}::{}", meta.name(), meta.method());
        method.call_raw(args)
    }

    async fn forward_async<A, B, C, D, Args, R, F>(
        &self,
        method: ForwardAsyncMethod<A, B, C, D, Args, R, F>,
        args: Args,
    ) -> ForwardReturn<R>
    where
        A: ForwardKind,
        B: Selector<Args, Output = A::Index>,
        C: ForwardKind,
        D: Selector<Args, Output = C::Index>,
        Args: Send,
        R: Send,
        F: Future<Output = R> + Send,
    {
        let meta = method.metadata();
        println!("→ local hop (async) :: {}::{}", meta.name(), meta.method());
        method.call_raw(args).await
    }
}

struct Target {
    something: String,
}

impl Target {
    fn raw_get_remote_something(&self) -> String {
        format!("raw {}", self.something)
    }

    async fn raw_async_get_remote(&self) -> String {
        format!("raw async {}", self.something)
    }
}

trait CrossSomething {
    fn get_remote_something(&self) -> String;
    async fn async_get_remote(&self) -> String;
}

impl CrossSomething for Target {
    fn get_remote_something(&self) -> String {
        self.raw_get_remote_something()
    }
    async fn async_get_remote(&self) -> String {
        self.raw_async_get_remote().await
    }
}
impl CrossSomething for ::forward::ForwardProxy<Target> {
    fn get_remote_something(&self) -> String {
        let config = ::forward::ForwardConfig {
            server: ::forward::ForwardSelector {
                kind: (),
                selector: (),
            },
            thread: ::forward::ForwardSelector {
                kind: (),
                selector: (),
            },
        };
        let meta = ::forward::ForwardMetadata::new("something1234", "get_remote_something");
        let method = ::forward::ForwardSyncMethod::new(config, meta, |args: (&Self,)| {
            let (target,) = args;
            target.as_raw_ref().get_remote_something()
        });
        let args = (self,);
        method.call_rpc(args).into_sync()
    }
    async fn async_get_remote(&self) -> String {
        let config = ::forward::ForwardConfig {
            server: ::forward::ForwardSelector {
                kind: (),
                selector: (),
            },
            thread: ::forward::ForwardSelector {
                kind: (),
                selector: (),
            },
        };
        let meta = ::forward::ForwardMetadata::new("something1234", "async_get_remote");
        let method = ::forward::ForwardAsyncMethod::new(config, meta, |args: (&Self,)| {
            let (target,) = args;
            target.as_raw_ref().async_get_remote()
        });
        let args = (self,);
        method.call_rpc(args).await.into_async().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn demo_forwarding_flow() {
        let proxy = ForwardProxy::new(Target {
            something: "payload".to_owned(),
        });

        let sync_value = proxy.get_remote_something();
        assert_eq!(sync_value, "raw payload");

        let async_value = proxy.async_get_remote().await;
        assert_eq!(async_value, "raw async payload");
    }
}
