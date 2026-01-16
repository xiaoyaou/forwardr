use std::{
    any::Any, cell::UnsafeCell, collections::HashMap, marker::PhantomData, sync::{LazyLock, Mutex}
};

type SyncMap = Mutex<UnsafeCell<HashMap<String, ForwardProxy>>>;

struct ForwardRegistry;

impl ForwardRegistry {
    
    /// 需要构建函数调用表来实现动态代理调用
    fn get_forward(proxy: &str, function: &str) -> Option<()> {
        static registry: LazyLock<SyncMap> = LazyLock::new(Default::default);

        let forward = registry.lock().unwrap().get_mut().get(proxy)?;
        
        Some(())// cast back to real type T and return ForwardMethod<T>
    }

}


// 前端代理 拦截函数调用，封装任务数据，向目标发送执行任务

// 后端代理 接受到新任务，还原任务数据，继续执行下一步代理。在最末端代理时即为抵达目标，直接本地执行，输出最终结果，封装后原路返回

// 可能需要枚举生成，或者使用VTable收集、查找目标方法并调用，这一步隐含了后端代理需要的函数签名信息

struct ForwardProxy {
    target: Box<dyn Any + Send>,
    methods: HashMap<String, ()>,
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    /// #[forward(name="something")]
    trait CrossSomething {
        /// #[proxyname="xxx"]
        fn get_remote_something(&self) -> String;

        // fn get_remote_async() -> impl Future<Output = String>;

        async fn async_get_remote() -> String;
    }

    struct Target {
        something: String,
    }

    impl CrossSomething for Target {
        fn get_remote_something(&self) -> String {
            self.something.clone()
        }

        // fn get_remote_async() -> impl Future<Output = String> {
        //     todo!()
        // }

        async fn async_get_remote() -> String {
            todo!()
        }
    }

    struct TargetForwardProxy {
        target: Target,
    }

    impl CrossSomething for TargetForwardProxy {
        fn get_remote_something(&self) -> String {
            // remote rpc or local dispatch
            // wrap(serialize) args
            let args = serialize();
            // RPC
            let f = socket_forward_remote("CrossSomething", "get_remote_something", args);

            block_on(f)
        }

        async fn async_get_remote() -> String {
            let args = serialize();

            let bytes = socket_forward_remote("CrossSomething", "async_get_remote", args).await;

            deserialize(bytes)
        }
    }

    fn serialize() -> Vec<u8> {
        Vec::new()
    }

    fn deserialize<T: Default>(_bytes: Vec<u8>) -> T {
        T::default()
    }

    fn block_on<T: Default>(_future: impl Future<Output = Vec<u8>>) -> T {
        // block until future is ready
        let bytes = Vec::new();
        // deserialize bytes into real return data
        deserialize(bytes)
    }

    struct ForwardPack<'a> {
        proxy: &'a str,
        function: &'a str,
        args: Vec<u8>,
        index: u8,
    }

    async fn socket_forward_remote(proxy: &str, function: &str, args: Vec<u8>) -> Vec<u8> {
        // prepare rpc package
        let forward_pack = ForwardPack {
            proxy, function, args, index: 0
        };
        // send rpc to remote and response
        send_mesage(forward_pack).await
    }

    async fn send_mesage(msg: ForwardPack<'_>) -> Vec<u8> {
        Vec::new()
    }
}
