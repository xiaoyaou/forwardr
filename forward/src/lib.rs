use conlock::oneshot;

pub use forward_macro::*;

pub struct ForwardProxy<T> {
    target: T,
}

impl<T> ForwardProxy<T> {
    pub fn new(target: T) -> Self {
        Self { target }
    }

    pub fn as_raw_ref(&self) -> &T {
        &self.target
    }

    pub fn as_raw_mut(&mut self) -> &mut T {
        &mut self.target
    }
}

#[derive(Clone, Copy)]
pub struct ForwardMetadata {
    name: &'static str,
    method: &'static str,
}

impl ForwardMetadata {
    pub const fn new(name: &'static str, method: &'static str) -> Self {
        Self { name, method }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn method(&self) -> &'static str {
        self.method
    }
}

pub enum ForwardReturn<T> {
    Raw(T),
    Forward(oneshot::Receiver<T>),
}

impl<T> ForwardReturn<T> {
    pub fn into_sync(self) -> T {
        match self {
            ForwardReturn::Raw(value) => value,
            ForwardReturn::Forward(mut receiver) => receiver
                .recv()
                .expect("forwarded sync call dropped before completion"),
        }
    }

    pub async fn into_async(self) -> T {
        match self {
            ForwardReturn::Raw(value) => value,
            ForwardReturn::Forward(receiver) => receiver
                .await
                .expect("forwarded async call dropped before completion"),
        }
    }
}

pub trait Forward {
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
        println!(
            "forward-sync :: {}::{} (fall back to raw)",
            meta.name(),
            meta.method()
        );
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
        println!(
            "forward-async :: {}::{} (fall back to raw)",
            meta.name(),
            meta.method()
        );
        method.call_raw(args).await
    }

    fn is_own_side(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct ForwardConfig<A, B, C, D> {
    pub server: ForwardSelector<A, B>,
    pub thread: ForwardSelector<C, D>,
}

#[derive(Clone)]
pub struct ForwardSelector<K, S> {
    pub kind: K,
    pub selector: S,
}

impl<K, S> ForwardSelector<K, S>
where
    K: ForwardKind,
{
    fn select<Args>(&self, args: &Args) -> K::Handle
    where
        S: Selector<Args, Output = K::Index>,
    {
        let index = self.selector.select(args);
        self.kind.select(index)
    }
}

pub trait ForwardKind {
    type Index: Index;
    type Handle: Forward;

    fn select(&self, index: Self::Index) -> Self::Handle;
}

pub trait Selector<Args> {
    type Output: Index;
    fn select(&self, args: &Args) -> Self::Output;
}

impl<Args, I, F> Selector<Args> for F
where
    I: Index,
    F: Fn(&Args) -> I,
{
    type Output = I;

    fn select(&self, args: &Args) -> Self::Output {
        (self)(args)
    }
}

impl<Args> Selector<Args> for () {
    type Output = ();

    fn select(&self, _: &Args) -> Self::Output {
        ()
    }
}

impl<Args> Selector<Args> for u32 {
    type Output = u32;

    fn select(&self, _: &Args) -> Self::Output {
        *self
    }
}

pub trait Index {}
impl Index for () {}
impl Index for u8 {}
impl Index for u16 {}
impl Index for u32 {}
impl Index for u64 {}

pub struct ForwardSyncMethod<A, B, C, D, Args, R>
where
    A: ForwardKind,
    B: Selector<Args, Output = A::Index>,
    C: ForwardKind,
    D: Selector<Args, Output = C::Index>,
{
    pub config: ForwardConfig<A, B, C, D>,
    pub meta: ForwardMetadata,
    pub method: fn(Args) -> R,
}

impl<A, B, C, D, Args, R> ForwardSyncMethod<A, B, C, D, Args, R>
where
    A: ForwardKind,
    B: Selector<Args, Output = A::Index>,
    C: ForwardKind,
    D: Selector<Args, Output = C::Index>,
{
    pub fn new(
        config: ForwardConfig<A, B, C, D>,
        meta: ForwardMetadata,
        method: fn(Args) -> R,
    ) -> Self {
        Self {
            config,
            meta,
            method,
        }
    }

    pub fn metadata(&self) -> ForwardMetadata {
        self.meta
    }

    pub fn call_rpc(self, args: Args) -> ForwardReturn<R> {
        let handle = self.config.server.select(&args);
        if handle.is_own_side() {
            return self.call_local(args);
        }
        handle.forward_sync(self, args)
    }

    pub fn call_local(self, args: Args) -> ForwardReturn<R> {
        let handle = self.config.thread.select(&args);
        if handle.is_own_side() {
            return self.call_raw(args);
        }
        handle.forward_sync(self, args)
    }

    pub fn call_raw(&self, args: Args) -> ForwardReturn<R> {
        ForwardReturn::Raw((self.method)(args))
    }
}

pub struct ForwardAsyncMethod<A, B, C, D, Args, R, F>
where
    A: ForwardKind,
    B: Selector<Args, Output = A::Index>,
    C: ForwardKind,
    D: Selector<Args, Output = C::Index>,
    F: Future<Output = R>,
{
    pub config: ForwardConfig<A, B, C, D>,
    pub meta: ForwardMetadata,
    pub method: fn(Args) -> F,
}

impl<A, B, C, D, Args, R, F> ForwardAsyncMethod<A, B, C, D, Args, R, F>
where
    A: ForwardKind,
    B: Selector<Args, Output = A::Index>,
    C: ForwardKind,
    D: Selector<Args, Output = C::Index>,
    F: Future<Output = R>,
{
    pub fn new(
        config: ForwardConfig<A, B, C, D>,
        meta: ForwardMetadata,
        method: fn(Args) -> F,
    ) -> Self {
        Self {
            config,
            meta,
            method,
        }
    }

    pub fn metadata(&self) -> ForwardMetadata {
        self.meta
    }

    pub async fn call_rpc(self, args: Args) -> ForwardReturn<R>
    where
        Args: Send,
        R: Send,
        F: Send,
    {
        let handle = self.config.server.select(&args);
        if handle.is_own_side() {
            return self.call_local(args).await;
        }
        handle.forward_async(self, args).await
    }

    pub async fn call_local(self, args: Args) -> ForwardReturn<R>
    where
        Args: Send,
        R: Send,
        F: Send,
    {
        let handle = self.config.thread.select(&args);
        if handle.is_own_side() {
            return self.call_raw(args).await;
        }
        handle.forward_async(self, args).await
    }

    pub async fn call_raw(&self, args: Args) -> ForwardReturn<R>
    where
        Args: Send,
        R: Send,
        F: Send,
    {
        ForwardReturn::Raw((self.method)(args).await)
    }
}

impl ForwardKind for () {
    type Index = ();
    type Handle = ();

    fn select(&self, _: Self::Index) -> Self::Handle {
        ()
    }
}

impl Forward for () {
    fn is_own_side(&self) -> bool {
        true
    }
}
