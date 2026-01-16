use std::{
    cell::UnsafeCell, future::poll_fn, marker::PhantomData, panic::AssertUnwindSafe,
    sync::LazyLock, task::Poll,
};

/// Worker单位，专门接收任务并执行，对应一个独立的异步Task
pub struct Worker {
    /// 任务队列（通道）
    queue: WorkerQueue,
    /// 底层工作Task句柄
    handle: tokio::task::JoinHandle<()>,
}

type Task = Box<dyn Future<Output = ()> + Send>;

impl Worker {
    /// 创建一个新的Worker实例
    pub fn new() -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Task>(1024);
        let handle = tokio::spawn(async move {
            while let Some(task) = receiver.recv().await {
                // let boxed = std::panic::AssertUnwindSafe(task);
                let mut pinned = Box::into_pin(task);
                if let Ok(err) = poll_fn(|cx| {
                    match std::panic::catch_unwind(AssertUnwindSafe(|| pinned.as_mut().poll(cx))) {
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
                        Err(err) => Poll::Ready(Err(err)),
                    }
                })
                .await
                {
                    eprintln!("task panic: {:?}", err);
                }
            }
        });
        Worker {
            queue: WorkerQueue { sender },
            handle,
        }
    }

    pub fn shared(&self) -> WorkerGuard<'_> {
        WorkerGuard::new(self.queue.clone())
    }

    pub fn abort(&self) {
        self.handle.abort();
    }
}

#[derive(Clone)]
struct WorkerQueue {
    sender: tokio::sync::mpsc::Sender<Task>,
}

/// 对外提供WorkerQueue访问，但是限制生命周期
pub struct WorkerGuard<'a> {
    queue: WorkerQueue,
    _marker: PhantomData<&'a ()>,
}

impl WorkerGuard<'_> {
    fn new(queue: WorkerQueue) -> Self {
        WorkerGuard {
            queue,
            _marker: PhantomData,
        }
    }

    /// 向Worker发送任务
    pub async fn send_task(
        &self,
        task: Task,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<Task>> {
        self.queue.sender.send(task).await
    }
}

pub struct WorkerManager {
    workers: UnsafeCell<Vec<Worker>>,
}

unsafe impl Sync for WorkerManager {}

impl WorkerManager {
    /// 创建一个新的WorkerManager实例
    fn new(num_workers: usize) -> Self {
        let mut workers = Vec::with_capacity(num_workers);
        for _ in 0..num_workers {
            workers.push(Worker::new());
        }
        WorkerManager {
            workers: UnsafeCell::new(workers),
        }
    }

    pub fn workers(&self) -> &Vec<Worker> {
        unsafe { &*self.workers.get() }
    }

    pub fn instance() -> &'static WorkerManager {
        static INSTANCE: LazyLock<WorkerManager> = LazyLock::new(|| {
            let cpu_num = std::thread::available_parallelism().map_or(4, |n| n.get());
            WorkerManager::new(cpu_num)
        });
        &INSTANCE
    }

    /// 关闭Workers
    pub unsafe fn terminate(&self) {
        // 自动drop Worker
        unsafe { &mut *self.workers.get() }.clear();
    }

    /// 获取指定的Worker
    pub fn get<'a>(&'a self, index: u32) -> WorkerGuard<'a> {
        self.workers()[index as usize].shared()
    }
}
