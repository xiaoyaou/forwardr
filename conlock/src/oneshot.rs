use std::{
    cell::UnsafeCell,
    future::Future,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
    task::Waker,
};

extern crate atomic_wait;

/// 状态同步的各bit位表示
enum BitStatus {
    /// bit0：数据就绪
    READY = 0b0001,
    /// bit1：管道关闭
    CLOSE = 0b0010,
    /// bit2：接收端正在等待，此bit与[`READY`] | [`CLOSE`]状态互斥
    WAITING = 0b0100,
    /// bit3：接收端已请求，此bit基于 [`WAITING`]（异步polling）或 [`READY`]（数据polled）具有不同含义
    POLLING = 0b1000,
}

use BitStatus::*;

impl BitStatus {
    #[inline(always)]
    const fn as_bit(self) -> u32 {
        self as u32
    }
}

trait Bit {
    fn set(self, bit: BitStatus) -> Self;

    fn unset(self, bit: BitStatus) -> Self;

    fn is(self, bit: BitStatus) -> bool;
}

impl Bit for u32 {
    #[inline(always)]
    fn set(self, bit: BitStatus) -> u32 {
        self | bit.as_bit()
    }
    #[inline(always)]
    fn unset(self, bit: BitStatus) -> u32 {
        self & !bit.as_bit()
    }
    #[inline(always)]
    fn is(self, bit: BitStatus) -> bool {
        self & bit.as_bit() != 0
    }
}

/// 原子变量的临时视图模型，用于简化/语义化state的原子操作
struct Status<'a> {
    state: &'a AtomicU32,
    current: u32,
}

impl<'a> Status<'a> {
    #[inline(always)]
    fn new(state: &'a AtomicU32) -> Self {
        Self {
            state,
            current: state.load(Ordering::Relaxed),
        }
    }
    #[inline(always)]
    fn wait_ready(&mut self) {
        atomic_wait::wait(self.state, self.current);
        self.current = self.state.load(Ordering::Relaxed);
    }
    #[inline(always)]
    fn wake_waiting(&self) {
        atomic_wait::wake_one(self.state);
    }
    #[inline(always)]
    fn is_ready(&self) -> bool {
        self.current.is(READY)
    }
    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.current.is(CLOSE)
    }
    #[inline(always)]
    fn is_waiting(&self) -> bool {
        self.current.is(WAITING)
    }
    #[inline(always)]
    fn is_polled(&self) -> bool {
        self.current.is(POLLING)
    }
    /// cas `Release`，可能虚假失败
    ///
    /// 设置ready，使用release同步data数据
    ///
    /// 取消waiting，用于后续receiver唤醒
    #[inline(always)]
    fn set_ready_release(&mut self) -> bool {
        match self.state.compare_exchange_weak(
            self.current,
            self.current.set(READY).unset(WAITING),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(new) => {
                self.current = new;
                false
            }
        }
    }
    /// cas `Acquire`，在[`READY`]状态下，强制取消ready标识
    ///
    /// 取消ready，用于后续data所有权转移
    ///
    /// 设置polled，标识data已被读（不可重复读）
    #[inline(always)]
    fn unset_ready_acquire(&mut self) {
        loop {
            match self.state.compare_exchange_weak(
                self.current,
                self.current.unset(READY).set(POLLING),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(new) => {
                    self.current = new;
                }
            }
        }
    }

    /// cas `Relaxed`，可能虚假失败
    ///
    /// 设置closed，标记通道单方面关闭
    ///
    /// 取消waiting，用于后续receiver唤醒
    #[inline(always)]
    fn set_closed(&mut self) -> bool {
        match self.state.compare_exchange_weak(
            self.current,
            self.current.set(CLOSE).unset(WAITING),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(new) => {
                self.current = new;
                false
            }
        }
    }

    /// cas `Relaxed`，可能虚假失败
    ///
    /// 设置waiting，标记recevier等待数据
    #[inline(always)]
    fn set_waiting(&mut self) -> bool {
        match self.state.compare_exchange_weak(
            self.current,
            self.current.set(WAITING),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(new) => {
                self.current = new;
                false
            }
        }
    }

    /// cas `Acquire`，[`WAITING`]状态下临时屏蔽waker，可能虚假失败
    ///
    /// 取消polling，在waiting状态下同步waker写入
    #[inline(always)]
    fn unset_polling_acquire(&mut self) -> bool {
        match self.state.compare_exchange_weak(
            self.current,
            self.current.unset(POLLING),
            Ordering::Acquire,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(new) => {
                self.current = new;
                false
            }
        }
    }

    /// cas `Release`，不存在虚假失败
    ///
    /// 仅限status(waiting & !polling)状态下设置polling，同步waker写入
    #[inline(always)]
    fn set_right_polling_release(&mut self) -> bool {
        match self.state.compare_exchange(
            self.current.set(WAITING).unset(POLLING),
            self.current.set(WAITING).set(POLLING),
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => true,
            Err(new) => {
                self.current = new;
                false
            }
        }
    }

    /// load fence
    #[inline(always)]
    fn acquire(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }
}

struct Inner<T> {
    state: AtomicU32,
    data: UnsafeCell<MaybeUninit<T>>,
    waker: UnsafeCell<Option<Waker>>,
}

impl<T> Inner<T> {
    #[inline(always)]
    unsafe fn write(&self, val: &T) {
        unsafe {
            let uninit = &mut *self.data.get();
            std::ptr::copy_nonoverlapping(val, uninit.as_mut_ptr(), 1);
        };
    }

    #[inline(always)]
    unsafe fn read(&self) -> T {
        unsafe { (&*self.data.get()).assume_init_read() }
    }

    #[inline(always)]
    const unsafe fn waker_mut(&self) -> &mut Option<Waker> {
        // SAFETY: 需要调用者确保waker的访问唯一性
        unsafe { &mut *self.waker.get() }
    }
}

struct DoubleRef<T> {
    ptr: NonNull<Inner<T>>,
}

impl<T> DoubleRef<T> {
    #[inline(always)]
    fn as_inner(&mut self) -> &Inner<T> {
        // SAFETY: `&mut self`保证唯一性，且只会存在共享引用
        unsafe { self.ptr.as_ref() }
    }

    #[inline(always)]
    unsafe fn take_acquire(&mut self) -> Inner<T> {
        // load fence
        let _ = self.as_inner().state.load(Ordering::Acquire);
        // SAFETY: `&mut self`保证唯一性，但需要调用者确保self是唯一的Inner持有者
        *unsafe { Box::from_raw(self.ptr.as_ptr()) }
    }

    #[inline(always)]
    fn drop<const WAKE: bool>(&mut self) {
        let inner = self.as_inner();
        let mut status = Status::new(&inner.state);
        loop {
            if status.is_closed() {
                // SAFETY: status(closed)保证了self是唯一持有者
                if status.is_ready() {
                    // SAFETY: status(ready)保证了数据有效，需要读出释放
                    unsafe { self.take_acquire().read() };
                } else {
                    unsafe { self.take_acquire() };
                }
                break;
            } else {
                if status.set_closed() {
                    // 只有sender才可能出现status(waiting & polling)
                    if WAKE && status.is_waiting() {
                        if status.is_polled() {
                            // load fence
                            let _ = status.acquire();
                            // SAFETY: status(waiting & polling)确保了waker只有sender访问
                            unsafe { inner.waker_mut() }.take().map(Waker::wake);
                        } else {
                            status.wake_waiting();
                        }
                    }
                    break;
                }
            }
        }
    }
}

/// 发送方，只能发送一次消息（所有权会消耗）
pub struct Sender<T> {
    channel: DoubleRef<T>,
}

/// 接收方，可以同步或者异步的接收消息，可以多次调用`recv`接收，但只能首次获取数据，后续皆返回None
pub struct Receiver<T> {
    channel: DoubleRef<T>,
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.channel.drop::<true>();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.channel.drop::<false>();
    }
}

impl<T> Sender<T> {
    /// 发送数据，数据发送完成后会尝试唤醒接收方的同步/异步等待，如果接收方已经销毁（关闭），则直接返回`Err(val)`
    pub fn send(mut self, val: T) -> Result<(), T> {
        let inner = self.channel.as_inner();
        let mut status = Status::new(&inner.state);
        if status.is_closed() {
            return Err(val);
        }
        // 预写准备
        // SAFETY: 唯一写入，且status(!ready)保证了接收方不会访问data（ready只在后面设置）
        // 注意：在ready确认之前，内存中字节层面会存在两份数据T，但由于预写内存为`MaybeUninit`，
        // 此块`T`内存只在status(ready)时确认有效，且原始T内存直接无效(`ManuallyDrop`)，完成所有权转移，
        // 所以此处预写不会导致T重复释放，或者不释放
        unsafe { inner.write(&val) };
        loop {
            // store fence，确认写入
            if status.set_ready_release() {
                let _ = ManuallyDrop::new(val);
                if status.is_waiting() {
                    // 唤醒接收端
                    if status.is_polled() {
                        // load fence，同步waker数据
                        let _ = status.acquire();
                        // SAFETY: status(polling)确保了waker已设置完成，接收端不会再访问
                        unsafe { inner.waker_mut() }.take().map(Waker::wake);
                    } else {
                        status.wake_waiting();
                    }
                }
                return Ok(());
            }
            if status.is_closed() {
                return Err(val);
            }
        }
    }
}

impl<T> Receiver<T> {
    /// 同步接收数据，如果数据未就绪，则进入阻塞状态等待唤醒，如果发送方已关闭（销毁）则直接返回None，重复的读取直接返回None
    pub fn recv(&mut self) -> Option<T> {
        let inner = self.channel.as_inner();
        let mut status = Status::new(&inner.state);
        loop {
            // 数据已就绪
            if status.is_ready() {
                // load fence，确认读出
                status.unset_ready_acquire();
                // SAFETY: status(ready)保证了数据有效
                // status(!waiting & polled)数据T所有权唯一且正常转移
                return Some(unsafe { inner.read() });
            }
            if status.is_closed() {
                return None;
            }
            // 虚假唤醒
            if status.is_waiting() {
                status.wait_ready();
                continue;
            }
            // status(!waiting & polled)：数据已被读取
            if status.is_polled() {
                return None;
            }
            // 进入waiting状态，等待唤醒
            if status.set_waiting() {
                status.wait_ready();
            };
        }
    }
}

/// 支持异步接收器
impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    /// 异步轮询接收器，如果数据已就绪则直接返回Ready，否则返回Pending等待再次调度，如果发送方已销毁（关闭）或者Future已完成，则直接返回Ready(None)
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner = self.get_mut().channel.as_inner();
        let mut status = Status::new(&inner.state);
        loop {
            // 数据已就绪
            if status.is_ready() {
                // load fence，确认读出
                status.unset_ready_acquire();
                // SAFETY: status(ready)保证了数据有效
                // status(!waiting & polled)数据T所有权唯一且正常转移
                return std::task::Poll::Ready(Some(unsafe { inner.read() }));
            }
            // 通道关闭
            if status.is_closed() {
                return std::task::Poll::Ready(None);
            }
            // 如果进入status(waiting)分支，通常意味着Future任务调度切换
            if status.is_waiting() {
                // 先屏蔽waker唤醒，防止sender正在访问waker
                if status.unset_polling_acquire() {
                    let will = unsafe { inner.waker_mut() }
                        .as_ref()
                        .map_or(false, |w| cx.waker().will_wake(w));
                    // 检查waker状态
                    if will {
                        return std::task::Poll::Pending;
                    }
                    // 更新waker，独占channel可变引用
                    // SAFETY: status(waiting & !polling)确保了sender不会访问waker
                    *unsafe { inner.waker_mut() } = Some(cx.waker().clone());
                    // 重新设置polling，同步新waker，为避免waker重复clone更新，此处不再引入虚假比较失败的可能
                    if status.set_right_polling_release() {
                        return std::task::Poll::Pending;
                    }
                }
                // 进入此处，意味着：
                // 1. unset虚假失败，需重新执行
                // 2. waiting状态有变(ready | closed)，只需重新检查 ready/closed 即可
                continue;
            }
            // status(!waiting & polled)：数据已被读取
            if status.is_polled() {
                return std::task::Poll::Ready(None);
            }
            // 确认等待
            if status.set_waiting() {
                // 写入waker，独占channel可变引用
                // SAFETY: status(waiting & !polling)确保了此处为首次写入，sender不会访问waker
                *unsafe { inner.waker_mut() } = Some(cx.waker().clone());
                // 设置轮询状态
                // 注意：为避免waker不必要的重复写入(clone)，此处不再引入虚假比较失败的情况（此分支最多执行一次）
                // 成功后，则正常进入轮询等待状态
                // 若失败，则意味着waiting状态有变(ready | closed)，只需重新检查 ready/closed 即可
                if status.set_right_polling_release() {
                    return std::task::Poll::Pending;
                }
            };
        }
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

/// 创建一对一次性的消息发送通道，可以发送一次消息并同步/异步的接收
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let ptr = Box::leak(Box::new(Inner {
        state: AtomicU32::new(0),
        data: UnsafeCell::new(MaybeUninit::uninit()),
        waker: UnsafeCell::new(None),
    }));
    (
        Sender {
            channel: DoubleRef { ptr: ptr.into() },
        },
        Receiver {
            channel: DoubleRef { ptr: ptr.into() },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fmt::Debug,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
        thread,
    };

    /// A simple type that increments a counter when dropped.
    struct DropCounter(Arc<AtomicUsize>);

    impl Debug for DropCounter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_tuple("DropCounter")
                .field(&self.0.load(Ordering::SeqCst))
                .finish()
        }
    }

    impl Drop for DropCounter {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Create a no-op RawWaker for polling the future manually.
    fn noop_raw_waker() -> RawWaker {
        static P: AtomicUsize = AtomicUsize::new(0);
        unsafe fn clone(p: *const ()) -> RawWaker {
            print!("waker clone: {:p}", p);
            noop_raw_waker()
        }
        unsafe fn wake(p: *const ()) {
            println!("waker wake: {:p}", p);
            unsafe { drop(p) };
        }
        unsafe fn wake_by_ref(p: *const ()) {
            println!("waker wake_by_ref: {:p}", p)
        }
        unsafe fn drop(p: *const ()) {
            println!("waker drop: {:p}", p)
        }

        const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
        let p = P.fetch_add(1, Ordering::Relaxed);
        println!(" -> new {}", p);
        RawWaker::new(std::ptr::without_provenance(p), &VTABLE)
    }

    fn noop_waker() -> Waker {
        unsafe { Waker::from_raw(noop_raw_waker()) }
    }

    #[test]
    fn basic_send_recv() {
        let (s, mut r) = channel();
        let counter = Arc::new(AtomicUsize::new(0));
        s.send(DropCounter(counter.clone())).unwrap();

        let got = r.recv().expect("should receive value");
        drop(got);

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn receiver_drop_before_send_returns_err_and_drops_value_locally() {
        let (s, r) = channel::<DropCounter>();
        drop(r);

        let counter = Arc::new(AtomicUsize::new(0));
        // send consumes `s` and will observe closed status and return Err(val)
        {
            let res = s.send(DropCounter(counter.clone()));
            // send should fail because receiver is dropped
            assert!(res.is_err());
        }
        // returned Err(val) is dropped at scope exit -> counter should be incremented
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_send_recv_threaded() {
        let (s, mut r) = channel();
        let counter = Arc::new(AtomicUsize::new(0));
        let sender_counter = counter.clone();

        let handle = thread::spawn(move || {
            // move s in and send
            s.send(DropCounter(sender_counter)).unwrap();
        });

        let got = r.recv().expect("should receive");
        drop(got);

        handle.join().unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn poll_pending_then_ready_after_send() {
        let (s, r) = channel();
        let mut r = Box::pin(r);

        let w = noop_waker();
        let mut cx = Context::from_waker(&w);

        // First poll should be Pending (no sender yet)
        match Pin::as_mut(&mut r).poll(&mut cx) {
            Poll::Pending => {}
            other => panic!("expected Pending, got {:?}", other),
        }
        match Pin::as_mut(&mut r).poll(&mut Context::from_waker(&noop_waker())) {
            Poll::Pending => {}
            other => panic!("expected Pending, got {:?}", other),
        }
        // Send from another thread, then poll again
        let counter = Arc::new(AtomicUsize::new(0));
        let send_counter = counter.clone();
        let handle = thread::spawn(move || {
            s.send(DropCounter(send_counter)).unwrap();
        });

        handle.join().expect("sender joined");
        // Now the channel should be ready
        match Pin::as_mut(&mut r).poll(&mut cx) {
            Poll::Ready(Some(_v)) => {
                // drop happens when leaving scope
            }
            other => panic!("expected Ready(Some), got {:?}", other),
        }
        match Pin::as_mut(&mut r).poll(&mut Context::from_waker(&noop_waker())) {
            Poll::Ready(None) => {}
            other => panic!("expected Ready(None), got {:?}", other),
        }
        // The value was dropped after being received
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
