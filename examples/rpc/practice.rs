use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::sync::oneshot;
use tokio::time::sleep;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A forwarding step describing how a request should travel through the runtime.
#[derive(Clone, Copy, Debug)]
enum Step {
    Rpc(&'static str),
    Thread(&'static str),
}

#[derive(Clone, Debug)]
struct ForwardPlan {
    steps: Arc<[Step]>,
}

impl ForwardPlan {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: steps.into(),
        }
    }

    fn steps(&self) -> Arc<[Step]> {
        Arc::clone(&self.steps)
    }

    fn len(&self) -> usize {
        self.steps.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    Client,
    Rpc(&'static str),
    Thread(&'static str),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Client => write!(f, "client"),
            Location::Rpc(name) => write!(f, "rpc::{name}"),
            Location::Thread(name) => write!(f, "thread::{name}"),
        }
    }
}

#[derive(Clone)]
struct ForwardRuntime {
    location: Location,
    registry: Arc<Registry>,
}

type AsyncHandler<Args, R> = Box<dyn FnOnce(Args) -> BoxFuture<R> + Send>;

fn into_async_handler<Args, R, F, Fut>(handler: F) -> AsyncHandler<Args, R>
where
    Args: Send + 'static,
    R: Send + 'static,
    F: FnOnce(Args) -> Fut + Send + 'static,
    Fut: Future<Output = R> + Send + 'static,
{
    let mut handler_opt = Some(handler);
    Box::new(move |args| {
        let inner = handler_opt
            .take()
            .expect("forward handler should be invoked exactly once");
        Box::pin(inner(args))
    })
}

impl ForwardRuntime {
    fn bootstrap() -> Self {
        let service = Arc::new(PlayerOpsImpl::new());
        let registry = Registry::new(service);
        ForwardRuntime {
            location: Location::Client,
            registry,
        }
    }

    fn execute_sync<Args, R, F>(
        &self,
        plan: &ForwardPlan,
        method: &'static str,
        args: Args,
        handler: F,
    ) -> R
    where
        Args: Send + 'static,
        R: Send + 'static,
        F: FnOnce(Args) -> R + Send + 'static,
    {
        self.exec_sync_cursor(plan.steps(), 0, method, args, handler)
    }

    async fn execute_async<Args, R, F, Fut>(
        &self,
        plan: &ForwardPlan,
        method: &'static str,
        args: Args,
        handler: F,
    ) -> R
    where
        Args: Send + 'static,
        R: Send + 'static,
        F: FnOnce(Args) -> Fut + Send + 'static,
        Fut: Future<Output = R> + Send + 'static,
    {
        self.exec_async_cursor(plan.steps(), 0, method, args, into_async_handler(handler))
            .await
    }

    fn exec_sync_cursor<Args, R, F>(
        &self,
        plan: Arc<[Step]>,
        index: usize,
        method: &'static str,
        args: Args,
        handler: F,
    ) -> R
    where
        Args: Send + 'static,
        R: Send + 'static,
        F: FnOnce(Args) -> R + Send + 'static,
    {
        if index >= plan.len() {
            println!("[{}] executing {method}", self.location);
            handler(args)
        } else {
            match plan[index] {
                Step::Rpc(server_name) => {
                    let next_index = index + 1;
                    let next_plan = Arc::clone(&plan);
                    if matches!(self.location, Location::Rpc(current) if current == server_name) {
                        self.exec_sync_cursor(next_plan, next_index, method, args, handler)
                    } else {
                        println!(
                            "[{}] forwarding {method} via RPC -> {server_name}",
                            self.location
                        );
                        let runtime = self.clone_for_rpc(server_name);
                        runtime.exec_sync_cursor(next_plan, next_index, method, args, handler)
                    }
                }
                Step::Thread(thread_name) => {
                    let next_index = index + 1;
                    let next_plan = Arc::clone(&plan);
                    if matches!(self.location, Location::Thread(current) if current == thread_name)
                    {
                        self.exec_sync_cursor(next_plan, next_index, method, args, handler)
                    } else {
                        println!(
                            "[{}] forwarding {method} to local executor {thread_name}",
                            self.location
                        );
                        let registry = Arc::clone(&self.registry);
                        let executor = registry
                            .threads
                            .get(thread_name)
                            .expect("thread registered for forwarding")
                            .clone();
                        executor.execute_sync(move || {
                            let runtime = ForwardRuntime {
                                location: Location::Thread(thread_name),
                                registry,
                            };
                            runtime.exec_sync_cursor(next_plan, next_index, method, args, handler)
                        })
                    }
                }
            }
        }
    }

    fn exec_async_cursor<Args, R>(
        &self,
        plan: Arc<[Step]>,
        index: usize,
        method: &'static str,
        args: Args,
        handler: AsyncHandler<Args, R>,
    ) -> BoxFuture<R>
    where
        Args: Send + 'static,
        R: Send + 'static,
    {
        if index >= plan.len() {
            println!("[{}] executing {method}", self.location);
            return handler(args);
        }
        match plan[index] {
            Step::Rpc(server_name) => {
                let next_index = index + 1;
                let next_plan = Arc::clone(&plan);
                if matches!(self.location, Location::Rpc(current) if current == server_name) {
                    self.exec_async_cursor(next_plan, next_index, method, args, handler)
                } else {
                    println!(
                        "[{}] asynchronously forwarding {method} via RPC -> {server_name}",
                        self.location
                    );
                    let runtime = self.clone_for_rpc(server_name);
                    runtime.exec_async_cursor(next_plan, next_index, method, args, handler)
                }
            }
            Step::Thread(thread_name) => {
                let next_index = index + 1;
                let next_plan = Arc::clone(&plan);
                if matches!(self.location, Location::Thread(current) if current == thread_name) {
                    self.exec_async_cursor(next_plan, next_index, method, args, handler)
                } else {
                    println!(
                        "[{}] asynchronously forwarding {method} to local executor {thread_name}",
                        self.location
                    );
                    let registry = Arc::clone(&self.registry);
                    let executor = registry
                        .threads
                        .get(thread_name)
                        .expect("thread registered for forwarding")
                        .clone();
                    Box::pin(async move {
                        executor
                            .execute_async(move || {
                                let runtime = ForwardRuntime {
                                    location: Location::Thread(thread_name),
                                    registry: Arc::clone(&registry),
                                };
                                runtime
                                    .exec_async_cursor(next_plan, next_index, method, args, handler)
                            })
                            .await
                    })
                }
            }
        }
    }

    fn clone_for_rpc(&self, server: &'static str) -> Self {
        let site = self
            .registry
            .rpc_sites
            .get(server)
            .unwrap_or_else(|| panic!("unknown rpc target: {server}"));
        println!("[{}] preparing remote context {}", self.location, site.name);
        ForwardRuntime {
            location: Location::Rpc(server),
            registry: Arc::clone(&self.registry),
        }
    }

    fn shutdown(&self) {
        self.registry.shutdown();
    }

    fn service(&self) -> Arc<PlayerOpsImpl> {
        Arc::clone(&self.registry.service)
    }
}

struct Registry {
    rpc_sites: HashMap<&'static str, RpcSite>,
    threads: HashMap<&'static str, LocalExecutor>,
    service: Arc<PlayerOpsImpl>,
}

impl Registry {
    fn new(service: Arc<PlayerOpsImpl>) -> Arc<Self> {
        let mut rpc_sites = HashMap::new();
        rpc_sites.insert(
            "cluster-alpha",
            RpcSite {
                name: "cluster-alpha",
            },
        );

        let mut threads = HashMap::new();
        threads.insert("worker-io", LocalExecutor::new("worker-io"));
        threads.insert("worker-async", LocalExecutor::new("worker-async"));

        Arc::new(Self {
            rpc_sites,
            threads,
            service,
        })
    }

    fn shutdown(&self) {
        for executor in self.threads.values() {
            executor.shutdown();
        }
    }
}

struct RpcSite {
    name: &'static str,
}

#[derive(Clone)]
struct LocalExecutor {
    name: &'static str,
    sender: Arc<mpsc::Sender<ThreadCommand>>,
}

impl LocalExecutor {
    fn new(name: &'static str) -> Self {
        let (tx, rx) = mpsc::channel::<ThreadCommand>();
        let sender = Arc::new(tx);
        let worker_sender = Arc::clone(&sender);
        let thread_name = name.to_string();
        thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                while let Ok(command) = rx.recv() {
                    match command {
                        ThreadCommand::Sync(job) => job(),
                        ThreadCommand::Async(job) => job(),
                        ThreadCommand::Shutdown => break,
                    }
                }
            })
            .expect("spawn local executor thread");
        LocalExecutor {
            name,
            sender: worker_sender,
        }
    }

    fn execute_sync<R, F>(&self, job: F) -> R
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        println!("[executor::{}] scheduling sync job", self.name);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let wrapped = Box::new(move || {
            let outcome = job();
            let _ = result_tx.send(outcome);
        });
        self.sender
            .send(ThreadCommand::Sync(wrapped))
            .expect("deliver sync task");
        result_rx.recv().expect("receive sync result")
    }

    fn execute_async<R, Fut, F>(&self, job: F) -> BoxFuture<R>
    where
        R: Send + 'static,
        Fut: Future<Output = R> + Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
    {
        println!("[executor::{}] scheduling async job", self.name);
        let (result_tx, result_rx) = oneshot::channel();
        let wrapped = Box::new(move || {
            let mut builder = TokioRuntimeBuilder::new_current_thread();
            builder.enable_all();
            let runtime = builder.build().expect("build dedicated local runtime");
            let future = job();
            let outcome = runtime.block_on(future);
            let _ = result_tx.send(outcome);
        });
        self.sender
            .send(ThreadCommand::Async(wrapped))
            .expect("deliver async task");
        Box::pin(async move { result_rx.await.expect("local executor stopped") })
    }

    fn shutdown(&self) {
        println!("[executor::{}] shutting down", self.name);
        let _ = self.sender.send(ThreadCommand::Shutdown);
    }
}

enum ThreadCommand {
    Sync(Box<dyn FnOnce() + Send>),
    Async(Box<dyn FnOnce() + Send>),
    Shutdown,
}

#[derive(Clone, Debug)]
struct PlayerProfile {
    id: u64,
    name: String,
    level: u32,
    score: i32,
}

impl PlayerProfile {
    fn new(id: u64, name: impl Into<String>, level: u32, score: i32) -> Self {
        Self {
            id,
            name: name.into(),
            level,
            score,
        }
    }
}

struct PlayerOpsImpl {
    store: Mutex<HashMap<u64, PlayerProfile>>,
}

impl PlayerOpsImpl {
    fn new() -> Self {
        let mut store = HashMap::new();
        store.insert(1001, PlayerProfile::new(1001, "Alice", 7, 120));
        store.insert(2002, PlayerProfile::new(2002, "Bob", 12, 980));
        store.insert(3003, PlayerProfile::new(3003, "Carol", 3, 45));
        PlayerOpsImpl {
            store: Mutex::new(store),
        }
    }

    fn fetch_profile(&self, player_id: u64) -> PlayerProfile {
        println!("[service] fetching profile for player {player_id}");
        let store = self.store.lock().expect("store poisoned");
        store
            .get(&player_id)
            .cloned()
            .unwrap_or_else(|| PlayerProfile::new(player_id, "Unknown", 1, 0))
    }

    async fn update_score_async(&self, player_id: u64, delta: i32) -> i32 {
        println!("[service] updating score for player {player_id} by delta {delta}");
        sleep(Duration::from_millis(75)).await;
        let mut store = self.store.lock().expect("store poisoned");
        let entry = store
            .entry(player_id)
            .or_insert_with(|| PlayerProfile::new(player_id, "AutoGenerated", 1, 0));
        entry.score += delta;
        entry.score
    }
}

#[derive(Clone)]
struct PlayerProxy {
    runtime: ForwardRuntime,
    service: Arc<PlayerOpsImpl>,
}

impl PlayerProxy {
    fn new(runtime: ForwardRuntime) -> Self {
        let service = runtime.service();
        PlayerProxy { runtime, service }
    }

    fn fetch_profile(&self, player_id: u64) -> PlayerProfile {
        let plan = ForwardPlan::new(vec![Step::Rpc("cluster-alpha"), Step::Thread("worker-io")]);
        let service = Arc::clone(&self.service);
        self.runtime
            .execute_sync(&plan, "PlayerOps::fetch_profile", player_id, move |id| {
                service.fetch_profile(id)
            })
    }

    async fn update_score(&self, player_id: u64, delta: i32) -> i32 {
        let plan = ForwardPlan::new(vec![
            Step::Rpc("cluster-alpha"),
            Step::Thread("worker-async"),
        ]);
        let service = Arc::clone(&self.service);
        self.runtime
            .execute_async(
                &plan,
                "PlayerOps::update_score",
                (player_id, delta),
                move |(id, change)| {
                    let service = Arc::clone(&service);
                    async move { service.update_score_async(id, change).await }
                },
            )
            .await
    }

    fn shutdown(&self) {
        self.runtime.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn forwarding_runtime_smoke_test() {
        let runtime = ForwardRuntime::bootstrap();
        let proxy = PlayerProxy::new(runtime);

        let profile = proxy.fetch_profile(1001);
        assert_eq!(profile.name, "Alice");
        assert_eq!(profile.score, 120);

        let new_score = proxy.update_score(1001, 55).await;
        assert_eq!(new_score, 175);

        let refreshed = proxy.fetch_profile(1001);
        assert_eq!(refreshed.score, 175);

        proxy.shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_forwarding_updates_are_serialized() {
        let runtime = ForwardRuntime::bootstrap();
        let proxy = PlayerProxy::new(runtime);
        let proxy_clone = proxy.clone();

        let fut_a = proxy.update_score(2002, 25);
        let fut_b = proxy_clone.update_score(2002, -15);
        let (score_a, score_b) = tokio::join!(fut_a, fut_b);

        let mut scores = [score_a, score_b];
        scores.sort();
        assert!(matches!(scores, [965, 990] | [990, 1005]));

        let final_profile = proxy.fetch_profile(2002);
        assert_eq!(final_profile.score, 990);

        proxy.shutdown();
    }
}
