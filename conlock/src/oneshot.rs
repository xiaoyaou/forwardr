use std::{
    mem::MaybeUninit,
    ptr::NonNull,
};

use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering::*},
};

#[cfg(feature = "wait")]
extern crate atomic_wait;

#[cfg(feature = "async")]
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// 原子状态位，各bit位表示和视图描述：
///
/// | READY | CLOSE | WAITING | FIELDLESS | PENDING |  Sender View  | Receiver View |
/// |:-----:|:-----:|:-------:|:---------:|:-------:|---------------|---------------|
/// |   0   |   0   |    0    |     0     |    0    | 初始状态，可发送 | 初始状态，需等待 |
/// |   1   |   0   |    0    |     0     |    0    | 发送完毕可以关闭 | 数据就绪可以读取 |
/// |   0   |   0   |    1    |     0     |    0    | 可发送、同步唤醒 | 同步等待数据就绪 |
/// |   0   |   0   |    1    |     0     |    1    | 可发送、异步唤醒 | 异步等待数据就绪 |
/// |   0   |   0   |    1    |     1     |    1    | 轮询中设置唤醒器 | 轮询中设置唤醒器 |
/// |   0   |   0   |    0    |     1     |    0    | 已读取，还未关闭 | 已读取，不可再读 |
/// |   0   |   1   |    0    |     0     |    0    | 接收关闭、不可发 | 发送关闭、不可读 |
/// |   0   |   1   |    0    |     1     |    0    | 预备销毁、可回收 | 预备销毁、可回收 |
/// |   1   |   1   |    0    |     1     |    0    | 发送销毁、已结束 | 发送销毁、可读取 |
#[repr(u32)]
enum BitStatus {
    /// bit0：数据就绪
    ///
    /// 此bit会清除等待状态
    READY = 0b0001,
    /// bit1：管道关闭
    ///
    /// 此bit会清除等待状态
    CLOSE = 0b0010,
    /// bit2：接收端正在同步等待
    ///
    /// 此bit与[`READY`] | [`CLOSE`]状态互斥
    WAITING = 0b0100,
    /// bit3：字段访问性标记
    ///
    /// 含义依赖于其他位的组合：
    /// - status(!CLOSE & !WAITING)：data已被读取，禁止重复读
    /// - status(CLOSE)：通道已标记可回收
    /// - status(PENDING)：异步特性下，waker正在初始化或修改中
    ///
    /// 详情见[`Status`]对应状态查询文档
    FIELDLESS = 0b1000,
    /// bit4：接收端正在异步等待
    ///
    /// 此bit只与[`WAITING`]状态共存，用于`async`特性下声明[`Poll::Pending`]
    #[cfg(feature = "async")]
    PENDING = 0b1_0000,
}

use crate::bit::{Bit, BitWith};
use crate::with;
use BitStatus::*;

impl Bit for BitStatus {
    fn bit(self) -> u32 {
        self as u32
    }
}

/// 原子变量的临时视图模型，用于简化/语义化state的原子操作
///
/// 状态副本status只在初始化时，以及CAS操作但失败时，才会同步更新致当前状态
struct Status<'a> {
    /// 真实的状态，原子类型
    state: &'a AtomicU32,
    /// 当前保留的临时状态，可能落后或同步于`state`的版本。
    /// 一般表现为CAS操作成功前的`state`副本
    current: u32,
}

/// 状态查询判断
impl Status<'_> {
    fn new(state: &AtomicU32) -> Status<'_> {
        Status {
            state,
            current: state.load(Relaxed),
        }
    }
    /// 检查副本状态：[`READY`]
    ///
    /// 数据已就绪
    #[inline(always)]
    fn is_ready(&self) -> bool {
        self.current.is(READY)
    }
    /// 检查副本状态：[`CLOSE`]
    ///
    /// 通道已关闭
    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.current.is(CLOSE)
    }
    /// 检查副本状态：[`WAITING`]
    ///
    /// 正在等待，只存在与 ![`READY`] + ![`CLOSE`] 状态下
    #[inline(always)]
    fn is_waiting(&self) -> bool {
        self.current.is(WAITING)
    }
    /// 检查副本状态：[`FIELDLESS`]，另见[`is_dropping`]、[`is_wakeless`]
    ///
    /// 数据已被读取，此状态必须依赖于 ![`READY`] + ![`CLOSE`] + ![`WAITING`] 状态，判断数据是否可读
    ///
    /// [`is_dropping`]: Status::is_dropping
    /// [`is_wakeless`]: Status::is_wakeless
    #[inline(always)]
    fn is_dataless(&self) -> bool {
        self.current.is(FIELDLESS)
    }
    /// 检查副本状态：[`FIELDLESS`]，另见[`is_dataless`]、[`is_wakeless`]
    ///
    /// 通道是否销毁，此状态必须依赖于[`CLOSE`]状态，判断通道是否可以销毁
    ///
    /// [`is_dataless`]: Status::is_dataless
    /// [`is_wakeless`]: Status::is_wakeless
    #[inline(always)]
    fn is_dropping(&self) -> bool {
        self.current.is(FIELDLESS)
    }
    /// 检查副本状态：[`FIELDLESS`]，另见[`is_dataless`]、[`is_dropping`]
    ///
    /// 在异步特性下，waker字段是否可访问，必须依赖于[`PENDING`]状态，判断waker是否可唤醒
    ///
    /// [`is_dataless`]: Status::is_dataless
    /// [`is_dropping`]: Status::is_dropping
    #[cfg(feature = "async")]
    #[inline(always)]
    fn is_wakeless(&self) -> bool {
        self.current.is(FIELDLESS)
    }
    /// 检查副本状态：[`PENDING`]
    ///
    /// 正在异步轮询，只存在与 [`WAITING`] 状态下
    #[cfg(feature = "async")]
    #[inline(always)]
    fn is_pending(&self) -> bool {
        self.current.is(PENDING)
    }
}

/// 状态cas宏，使用`+`、`-`语义原子修改状态，副本状态视情况同步更新，可以保留原始状态由于判断操作前的状态
macro_rules! cas {
    // 强CAS操作，切换状态，仅失败后更新当前副本状态
    ($ordering:ident : $self:ident $($rest:tt)+ ) => {{
        let new = with!($self.current, $($rest)+);
        match $self.state.compare_exchange($self.current, new, $ordering, Relaxed) {
            Ok(_) => true,
            Err(new) => {
                $self.current = new;
                false
            }
        }
    }};
    // 弱CAS操作，切换状态，仅失败后更新当前副本状态
    (weak $ordering:ident : $self:ident $($rest:tt)+ ) => {{
        let new = with!($self.current, $($rest)+);
        match $self.state.compare_exchange_weak($self.current, new, $ordering, Relaxed) {
            Ok(_) => true,
            Err(new) => {
                $self.current = new;
                false
            }
        }
    }};
    // 强CAS操作，切换状态，同步更新当前副本状态
    ($ordering:ident = $self:ident $($rest:tt)+ ) => {{
        let new = with!($self.current, $($rest)+);
        match $self.state.compare_exchange($self.current, new, $ordering, Relaxed) {
            Ok(_) => {
                $self.current = new;
                true
            },
            Err(new) => {
                $self.current = new;
                false
            }
        }
    }};
    // 弱CAS操作，切换状态，同步更新当前副本状态
    (weak $ordering:ident = $self:ident $($rest:tt)+ ) => {{
        let new = with!($self.current, $($rest)+);
        match $self.state.compare_exchange_weak($self.current, new, $ordering, Relaxed) {
            Ok(_) => {
                $self.current = new;
                true
            },
            Err(new) => {
                $self.current = new;
                false
            }
        }
    }};
}
/// 同步等待
#[cfg(feature = "sync")]
impl Status<'_> {
    /// 进入同步阻塞，等待数据就绪，可能存在虚假唤醒
    #[cfg(feature = "wait")]
    fn wait_ready(&mut self) {
        atomic_wait::wait(self.state, self.current);
        self.current = self.state.load(Relaxed);
    }
    /// 进入自旋，等待数据就绪或者状态有变
    #[cfg(not(feature = "wait"))]
    fn wait_ready(&mut self) {
        self.current = loop {
            let new_state = self.state.load(Relaxed);
            if new_state != self.current {
                break new_state;
            }
            std::hint::spin_loop();
        }
    }
    /// 唤醒所有等待的receiver线程
    #[cfg(feature = "wait")]
    fn wake_waiting(&self) {
        atomic_wait::wake_all(self.state);
    }
}

/// 状态修改原子操作
///
/// CAS方法命名规则：
/// 1. 含有try：weak cas，允许虚假失败；
///     否则strong cas，保证成功或失败的准确性
/// 2. 含有sync：无论成功与否都会同步更新当前的状态副本；
///     否则只会在失败时更新，成功后保留旧值用于判断原始状态
///     通常是在连续cas且中间不需要判断初始状态时，用于始终同步状态副本
/// 3. 使用set/unset：表示确定性状态变更，只影响方法签名中给定的状态
///     主要用来明确当前cas的预期状态变化
/// 4. 使用setup：表示组合状态变更，可能同时修改多个相互关联的状态位（共存或互斥约束）
///     通常在cas状态变更较复杂时使用，涉及到多种状态约束，方法签名中只给出主要变化目的
/// 5. 含有release：成功时使用Release语义，失败时使用Relaxed语义；否则默认都是Relaxed语义
/// 6. 含有acquire：成功时使用Acquire语义，失败时使用Relaxed语义；否则默认都是Relaxed语义
impl Status<'_> {
    /// 提供 load `Acquire` 屏障
    #[inline(always)]
    fn acquire(&self) {
        // 使用原子变量的load操作来实现Acquire屏障，在高竞争环境更高效
        self.state.load(Acquire);
    }
    /// 设置ready、取消waiting、pending、fieldless，确认data数据就绪，并准备唤醒receiver
    #[cfg(feature = "async")]
    fn try_setup_ready_release(&mut self) -> bool {
        // 异步状态下，需要同时清除所有等待相关的标记位
        cas!(weak Release: self + READY - WAITING - FIELDLESS - PENDING)
    }
    /// 设置ready、取消waiting，确认data数据就绪，并准备唤醒receiver
    #[cfg(not(feature = "async"))]
    fn try_setup_ready_release(&mut self) -> bool {
        // 同步状态下，只需要重置waiting状态即可
        cas!(weak Release: self + READY - WAITING)
    }
    /// 取消ready、设置dataless，获取data所有权（不可重复读）
    fn try_setup_dataless_acquire(&mut self) -> bool {
        cas!(weak Acquire: self - READY + FIELDLESS)
    }
    /// receiver销毁时清理data，取消ready状态，这里不能设置fieldless，因为可能closed
    fn try_sync_unset_ready_acquire(&mut self) -> bool {
        cas!(weak Acquire = self - READY)
    }
    /// 设置closed，配合fieldless直接进入可回收状态；取消waiting、pending，维护正确状态
    #[cfg(feature = "async")]
    fn try_setup_closed_dropping_release(&mut self) -> bool {
        // 同时清除所有等待相关的标记位，FIELDLESS根据前置条件已知已设置，配合CLOSE直接进入dropping状态
        cas!(weak Release: self + CLOSE - WAITING - PENDING)
    }
    /// 设置closed、取消waiting，标记通道单方面关闭，并唤醒等待线程
    fn try_sync_setup_closed_wake(&mut self) -> bool {
        cas!(weak Relaxed = self + CLOSE - WAITING)
    }
    /// 设置closed，标记通道单方面关闭；取消waiting、pending，用于后续waker唤醒
    #[cfg(feature = "async")]
    fn try_sync_setup_closed_acquire(&mut self) -> bool {
        // 同时清除所有等待相关的标记位，FIELDLESS根据前置条件已知未设置
        cas!(weak Acquire = self + CLOSE - WAITING - PENDING)
    }
    /// 设置closed、dropping，标记通道单方面关闭并且可以回收
    fn try_set_closed_dropping_release(&mut self) -> bool {
        cas!(weak Release: self + CLOSE + FIELDLESS)
    }
    /// 设置dropping，标记通道预备回收
    fn try_set_dropping_release(&mut self) -> bool {
        cas!(weak Release: self + FIELDLESS)
    }
    /// 设置waiting，标记receiver等待数据
    #[cfg(feature = "sync")]
    fn try_set_waiting(&mut self) -> bool {
        cas!(weak Relaxed = self + WAITING)
    }
    /// 设置waiting、pending、wakeless，标记异步轮询开始，并屏蔽waker访问
    #[cfg(feature = "async")]
    fn try_sync_set_waiting_pending_wakeless(&mut self) -> bool {
        cas!(weak Relaxed = self + WAITING + PENDING + FIELDLESS)
    }
    /// 取消pending，使异步轮询转换为同步等待，准备唤醒waker
    #[cfg(feature = "sync")]
    #[cfg(feature = "async")]
    fn try_unset_pending_acquire(&mut self) -> bool {
        cas!(weak Acquire: self - PENDING)
    }
    /// 设置wakeless，屏蔽waker访问
    #[cfg(feature = "async")]
    fn try_sync_set_wakeless_acquire(&mut self) -> bool {
        cas!(weak Acquire = self + FIELDLESS)
    }
    /// 取消wakeless，释放waker访问权
    #[cfg(feature = "async")]
    fn unset_wakeless_release(&mut self) -> bool {
        cas!(Release: self - FIELDLESS)
    }
}

/// 内部共享数据结构，自身无需Drop，数据内容完全通过`state`状态管理
struct Inner<T> {
    state: AtomicU32,
    data: UnsafeCell<MaybeUninit<T>>,
    #[cfg(feature = "async")]
    waker: UnsafeCell<MaybeUninit<Waker>>,
}

impl<T> Inner<T> {
    fn new() -> Self {
        Inner {
            state: AtomicU32::new(0),
            data: UnsafeCell::new(MaybeUninit::uninit()),
            #[cfg(feature = "async")]
            waker: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    /// 回收通道内存，必须保证调用后没有任何对Inner<T>的访问，包括它的引用
    unsafe fn dealloc(&self) {
        let raw = self as *const Inner<T> as *mut Inner<T>;
        // SAFETY: 只有sender和receiver都销毁后，才能回收Inner<T>，需要保证不会再访问Inner<T>
        drop(unsafe { Box::from_raw(raw) });
    }
    unsafe fn write(&self, val: T) {
        unsafe {
            let uninit = &mut *self.data.get();
            uninit.write(val);
        }
    }
    unsafe fn read(&self) -> T {
        unsafe {
            let init = &*self.data.get();
            init.assume_init_read()
        }
    }
    #[cfg(feature = "async")]
    const unsafe fn waker_mut(&self) -> &mut MaybeUninit<Waker> {
        // SAFETY: 需要调用者确保waker的访问唯一性
        unsafe { &mut *self.waker.get() }
    }
    /// => status(ready)：尝试唤醒等待的receiver
    #[allow(unused_variables)]
    fn try_wake_after_ready(&self, status: &Status) {
        #[cfg(feature = "async")]
        // 异步等待，需要考虑waker唤醒
        if status.is_pending() {
            // receiver已经在异步等待，这里需要唤醒waker
            if !status.is_wakeless() {
                // load fence，同步receiver曾经对waker的写入
                // 与`unset_wakeless_release`形成同步关系，确保能正确读出waker
                status.acquire();
                // SAFETY: status(pending & !wakeless)确保了waker已设置完成，
                // status(ready) => status(!pending) 确保了waker只会被唤醒这一次
                unsafe { self.waker_mut().assume_init_read() }.wake();
            }
            // 否则receiver正在轮询中设置waker
            // 因为status(ready)强制切换，后续receiver会重新维护状态
            return;
        }
        #[cfg(feature = "wait")]
        // 只是同步等待
        // 正在阻塞等待
        if status.is_waiting() {
            status.wake_waiting();
        }
    }
    /// 发送数据，只能调用一次
    fn send(&self, val: T) -> Result<(), T> {
        let mut status = Status::new(&self.state);
        if status.is_closed() {
            return Err(val);
        }
        // 预写准备，因为send只会调用一次，所以这里不需要做额外检查
        // SAFETY: 首次唯一写入，且status(!ready)保证了接收方不会访问data，因为ready只在后面设置
        unsafe { self.write(val) };
        loop {
            // store fence，确认写入，清除等待状态便于唤醒
            // 与`try_setup_dataless_acquire`、`try_sync_unset_ready_acquire`形成同步关系，
            // 确保receiver能正确读出数据
            if status.try_setup_ready_release() {
                self.try_wake_after_ready(&status);
                return Ok(());
            }
            if status.is_closed() {
                // SAFETY: ready设置失败，但我们知道初始化已完成，所以重新读出数据返回
                return Err(unsafe { self.read() });
            }
        }
    }
    /// 同步接收数据，可以同时存在多个调用，直到数据就绪或者数据不可读，可以多线程调用
    #[cfg(feature = "sync")]
    fn recv(&self) -> Option<T> {
        let mut status = Status::new(&self.state);
        loop {
            // 数据已就绪
            if status.is_ready() {
                // load fence，确认读出
                // 与`try_setup_ready_release`形成同步关系，确保receiver能正确读出数据
                if status.try_setup_dataless_acquire() {
                    // SAFETY: status(ready)保证数据有效
                    // => status(!ready & dataless)表明已获取数据T持有权
                    return Some(unsafe { self.read() });
                }
                // 读出失败（虚假可能），重新检查状态
                continue;
            }
            // 无数据且通道关闭
            if status.is_closed() {
                return None;
            }
            // 已经在等待状态，重新进入阻塞状态
            if status.is_waiting() {
                #[cfg(feature = "async")]
                // 异步环境下，异步可能强制切换为同步等待，所以需要考虑waker唤醒
                if status.is_pending() {
                    // load fence，取消异步状态，同步对waker的写入
                    // 与`unset_wakeless_release`形成同步关系，确保能正确访问waker
                    if status.try_unset_pending_acquire() {
                        // 异步切同步，需要删除waker，使异步调度器有感知
                        // SAFETY: status(waiting & pending)确保了waker已设置
                        // => status(!pending) 确保waker只会被处理这一次
                        unsafe { self.waker_mut().assume_init_drop() };
                    } else {
                        // 切换失败（虚假可能），重新检查状态
                        continue;
                    }
                }
                status.wait_ready();
                continue;
            }
            // status(!ready & !waiting & dataless)：数据已被读取
            if status.is_dataless() {
                return None;
            }
            // 原子进入waiting状态，等待唤醒
            if status.try_set_waiting() {
                status.wait_ready();
            };
        }
    }
    /// 异步轮询接收数据，只能同时存在一个轮询调用，直到数据就绪或者数据不可读，可以多线程调用
    #[cfg(feature = "async")]
    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut status = Status::new(&self.state);
        loop {
            // 数据已就绪
            if status.is_ready() {
                // load fence，确认读出，同步data写入
                // 与`try_setup_ready_release`形成同步关系，确保数据就绪
                if status.try_setup_dataless_acquire() {
                    // SAFETY: status(ready)保证了数据有效
                    // => status(!ready & dataless)表明已获取数据T持有权
                    return Poll::Ready(Some(unsafe { self.read() }));
                }
                // 读出失败（虚假可能），重新检查状态
                continue;
            }
            // 无数据且通道关闭
            if status.is_closed() {
                return Poll::Ready(None);
            }
            // 注意：poll_recv(Future::poll)独占receiver，所以异步环境下无需考虑多个接收方竞争的问题

            // 如果满足status(waiting)，通常意味着Future任务调度器切换
            if status.is_waiting() {
                // 隐含了 => status(waiting & pending & !wakeless)
                // 原子获取访问权，屏蔽waker，防止sender竞态访问
                // load fence：同步上次轮询对waker的写入（可能在不同调度器）
                // 与`unset_wakeless_release`形成同步关系，确保能正确访问waker
                if status.try_sync_set_wakeless_acquire() {
                    // SAFETY: status(waiting)说明了waker有效，status(wakeless)确保waker只有此处访问
                    let waker = unsafe { self.waker_mut().assume_init_mut() };
                    // 检查waker是否变更
                    if !waker.will_wake(cx.waker()) {
                        // 更新waker，释放旧数据
                        *waker = cx.waker().clone();
                    }
                    // store fence：取消wakeless，同步waker。为避免极端情况多次clone，此处不再引入虚假失败的可能
                    if status.unset_wakeless_release() {
                        return Poll::Pending;
                    }
                    // 进入此处，意味着当前状态被强制切换为：
                    // => status(ready | closed)，所以需要receiver就地销毁waker
                    unsafe { self.waker_mut().assume_init_drop() }
                }
                // 进入此处，意味着：
                // 1. wakeless切换失败（虚假可能），状态被强制变更，需重新检查状态
                // 2. wakeless释放失败，只能是waiting强制切换(ready | closed)，重新检查即可
                continue;
            }
            // status(!ready & !waiting & dataless)：数据已被读取
            if status.is_dataless() {
                return Poll::Ready(None);
            }
            // 原子确认等待，并且屏蔽waker，避免sender访问waker
            // 注意：这里使用Relaxed原子序，是因为根据status状态表，只有此处receiver可以初始化waker
            if status.try_sync_set_waiting_pending_wakeless() {
                // 首次写入waker
                // SAFETY: status(waiting & wakeless)确保了此处为首次写入，sender不会访问waker
                unsafe { self.waker_mut() }.write(cx.waker().clone());
                // store fence：释放waker，同步写入
                // 成功后，则正常进入轮询等待状态
                // 若失败，则意味着waiting状态有变(ready | closed)，只需重新检查 ready/closed 即可
                // 注意：为避免不必要的重复写入和clone，此处不再引入虚假失败的情况
                if status.unset_wakeless_release() {
                    return Poll::Pending;
                }
                // 进入此处，意味着已经是：
                // => status(ready | closed)，所以需要receiver就地销毁waker
                unsafe { self.waker_mut().assume_init_drop() };
            }
            // 等待状态切换失败（虚假可能），重新检查状态
            continue;
        }
    }
    /// 销毁sender，关闭通道，必要时，唤醒等待的receiver，释放通道资源
    fn drop_sender(&self) {
        let mut status = Status::new(&self.state);
        loop {
            // status(closed)：receiver已关闭
            if status.is_closed() {
                // status(closed & dropping)：receiver销毁结束，预备回收
                if status.is_dropping() {
                    // load fence，确保销毁在最后
                    // 与`try_set_closed_dropping_release`、`try_set_dropping_release`形成同步关系，
                    // 确保通道销毁时，receiver对通道的访问已结束
                    status.acquire();
                    // SAFETY: status(closed & dropping) 准备回收，Inner已不再被访问
                    unsafe { self.dealloc() };
                    break;
                }

                // => status(closed & dropping)：标记可回收
                // store fence，确保之前的访问已完成
                if status.try_set_dropping_release() {
                    break;
                }
                // 1. 虚假失败，重新检查状态
                // 2. receiver修改了状态，这里重新检查状态执行回收
                continue;
            }
            #[cfg(feature = "async")]
            // status(waiting & pending)：receiver正在异步等待，需要考虑waker唤醒
            if status.is_pending() {
                // receiver正在轮询中设置waker
                if status.is_wakeless() {
                    // 因为触发status(ready | closed)强制切换，后续receiver会重新维护状态
                    if status.try_setup_closed_dropping_release() {
                        // (FIELDLESS) + CLOSE - WAITING - PENDING
                        break;
                    }
                    // 变更失败（虚假可能），重新检查状态
                } else {
                    // status(!wakeless)：waker设置完成，准备唤醒
                    // load fence，确认通道关闭，准备唤醒waker
                    // 与`unset_wakeless_release`形成同步关系，确保能正确访问waker
                    if status.try_sync_setup_closed_acquire() {
                        // + CLOSE - WAITING - PENDING
                        // SAFETY: status(pending & !wakeless)确保了waker已设置完成，
                        // => status(!pending) 确保了waker只会被唤醒这一次
                        unsafe { self.waker_mut().assume_init_read() }.wake();
                        // store fence，确保之前的访问已完成
                        if status.try_set_dropping_release() {
                            break;
                        }
                        // 进入这里，意味着：
                        // 1. 虚假失败
                        // 2. receiver修改了状态
                        // 重新进入closed分支做回收检查即可
                    }
                }
                // 重新检查状态
                continue;
            }
            // status(waiting & !pending)：receiver正在同步等待
            if status.is_waiting() {
                // 原子确认通道关闭，准备唤醒
                if status.try_sync_setup_closed_wake() {
                    // + CLOSE - WAITING
                    #[cfg(feature = "wait")]
                    // 同步等待状态下需要唤醒
                    // 唤醒阻塞等待
                    status.wake_waiting();
                    // store fence，确保之前的访问已完成
                    if status.try_set_dropping_release() {
                        break;
                    }
                    // 重新进入closed分支做回收检查即可
                }
                // 状态变更失败（虚假可能），重新检查状态
                continue;
            }
            // receiver即没关闭也没等待，可以直接销毁
            // store fence，确保之前的访问已完成
            if status.try_set_closed_dropping_release() {
                break;
            }
            // 重新检查状态
        }
    }
    /// 销毁receiver，关闭通道，必要时，销毁数据，唤醒等待的waker，释放通道资源
    fn drop_receiver(&self) {
        let mut status = Status::new(&self.state);
        loop {
            // 这里做了一个小改动：优先检查并清理数据（先转化为未就绪状态），再从新状态处理drop，没有严格按照状态转移表处理
            // 好处是，状态切换次数没有变，但是可以减少一些分支判断次数
            if status.is_ready() {
                // load fence，同步data写入
                // 与`try_setup_ready_release`形成同步关系，确保数据正确读出
                if !status.try_sync_unset_ready_acquire() {
                    continue;
                }
                // SAFETY: status(ready)保证了数据有效
                unsafe { self.read() };
                // 这里虽然清理了data数据，但是并不设置dataless，因为：
                // 1. 当前receiver已经在drop阶段，数据只有这里能访问（清理），所以不需要设置dataless来禁止重复读
                // 2. 当前可能正好是closed，如果设置了dataless，就会误入status(closed & dropping)状态，导致sender误判回收逻辑
            }
            // status(closed)：sender已关闭
            if status.is_closed() {
                if status.is_dropping() {
                    // load fence，确保销毁在最后
                    // 与
                    // `try_set_dropping_release`、
                    // `try_set_closed_dropping_release`、
                    // `try_setup_closed_dropping_release`
                    // 形成同步关系，确保通道销毁时，sender对通道的访问已结束
                    status.acquire();
                    // SAFETY: status(closed & dropping) 准备回收，Inner已不再被访问
                    unsafe { self.dealloc() };
                    break;
                }

                // => status(closed & dropping)：标记可回收
                // store fence，确保之前的访问已完成
                if status.try_set_dropping_release() {
                    break;
                }
                // 1. 虚假失败，重新检查状态
                // 2. receiver标记了可回收，这里重新检查状态执行回收
                continue;
            }
            #[cfg(feature = "async")]
            // 异步任务未完成，需要考虑waker处理
            if status.is_pending() {
                // => status(waiting & pending & !wakeless)：隐含了这些状态，因为当前是receiver的drop阶段
                // load fence，原子确认关闭，确认处理waker
                // 与`unset_wakeless_release`形成同步关系，确保能正确访问waker
                if status.try_sync_setup_closed_acquire() {
                    // + CLOSE - WAITING - PENDING
                    // receiver已销毁，不需要再轮询，直接销毁waker
                    // SAFETY: status(pending & !wakeless)确保了waker已设置完成，
                    // => status(!pending) 确保了waker只会被处理这一次
                    unsafe { self.waker_mut().assume_init_drop() };
                    if status.try_set_dropping_release() {
                        // + FIELDLESS
                        break;
                    }
                    // 进入这里，意味着：
                    // 1. 虚假失败
                    // 2. receiver修改了状态
                    // 重新进入closed分支做回收检查即可
                }
                // 重新检查状态
                continue;
            }
            // 数据已清理，可以直接销毁
            // store fence，确保之前的访问已完成
            if status.try_set_closed_dropping_release() {
                break;
            }
        }
    }
}

/// 双重引用结构，Sender和Receiver共享同一Inner<T>实例
struct BiRef<T> {
    ptr: NonNull<Inner<T>>,
}

impl<T> BiRef<T> {
    const fn as_inner(&self) -> &Inner<T> {
        // SAFETY: 只会存在共享引用&Inner<T>，且Inner<T>的内部使用原子操作保证线程安全
        unsafe { self.ptr.as_ref() }
    }
}

/// 发送方，最多只能成功发送一次消息
pub struct Sender<T> {
    channel: BiRef<T>,
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.channel.as_inner().drop_sender()
    }
}

/// 接收方，可以同步或者异步（需要开启异步特性）的接收消息，消息数据最多只能成功被接收一次。
/// 可以多次（多线程）调用`recv`接收，但只能单次获取数据，后续皆返回None。
pub struct Receiver<T> {
    channel: BiRef<T>,
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.channel.as_inner().drop_receiver()
    }
}

impl<T> Sender<T> {
    /// 发送数据，数据发送完成后会尝试唤醒接收方的同步/异步等待，如果接收方已经关闭（销毁），则直接返回`Err(val)`
    pub fn send(self, val: T) -> Result<(), T> {
        self.channel.as_inner().send(val)
    }
}

#[cfg(feature = "sync")]
impl<T> Receiver<T> {
    /// 同步接收数据，如果数据未就绪，则进入阻塞状态等待唤醒，如果发送方已关闭（销毁）则直接返回None，重复的读取直接返回None
    pub fn recv(&self) -> Option<T> {
        self.channel.as_inner().recv()
    }
}

/// 支持异步接收器
#[cfg(feature = "async")]
impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    /// 异步轮询接收器，如果数据已就绪则直接返回Ready，否则返回Pending等待再次调度，
    /// 如果发送方已销毁（关闭）或者Future已结束（再次轮询），则直接返回Ready(None)
    ///
    /// 注意：签名中的`&mut Self`已经保证了Receiver的独占性，所以无需担心多线程竞争问题
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().channel.as_inner().poll_recv(cx)
    }
}

/// 数据类型T本身是Send时，发送方可以安全地在线程间传递
unsafe impl<T> Send for Sender<T> {}
/// 数据类型T本身是Send时，接收方可以安全地在线程间传递
unsafe impl<T: Send> Send for Receiver<T> {}
/// 数据类型T本身是Send时，接收方可以安全地在线程间共享。
/// T不必是Sync的，因为T不会被共享，只有一个线程能成功接收T
unsafe impl<T: Send> Sync for Receiver<T> {}

/// 创建一对一次性的消息发送通道，可以发送一次消息并同步/异步的接收
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let ptr = Box::leak(Box::new(Inner::new()));
    (
        Sender {
            channel: BiRef {
                ptr: NonNull::from_mut(ptr),
            },
        },
        Receiver {
            channel: BiRef {
                ptr: NonNull::from_mut(ptr),
            },
        },
    )
}

#[cfg(test)]
#[cfg(feature = "sync")]
mod sync_tests {
    use super::*;

    #[test]
    fn test_oneshot_basic() {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            tx.send(42).unwrap();
        });
        assert_eq!(rx.recv(), Some(42));
    }

    #[test]
    fn test_oneshot_send_then_recv() {
        let (tx, rx) = channel();
        tx.send(100).unwrap();
        assert_eq!(rx.recv(), Some(100));
    }

    #[test]
    fn test_oneshot_recv_before_send() {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            tx.send("hello").unwrap();
        });
        assert_eq!(rx.recv(), Some("hello"));
    }

    #[test]
    fn test_oneshot_duplicate_recv() {
        let (tx, rx) = channel();
        tx.send(42).unwrap();
        assert_eq!(rx.recv(), Some(42));
        // 重复接收应返回 None
        assert_eq!(rx.recv(), None);
    }

    #[test]
    fn test_oneshot_sender_dropped() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        // sender 销毁，接收方应返回 None
        assert_eq!(rx.recv(), None);
    }

    #[test]
    fn test_oneshot_receiver_dropped_before_send() {
        let (tx, rx) = channel();
        drop(rx);
        // receiver 销毁，send 应返回 Err
        assert_eq!(tx.send(42), Err(42));
    }

    #[test]
    fn test_oneshot_receiver_dropped_after_send() {
        let (tx, rx) = channel();
        tx.send(42).unwrap();
        drop(rx);
    }

    #[test]
    fn test_oneshot_multiple_threads_recv() {
        let (tx, rx) = channel();

        std::thread::spawn(move || tx.send(99).unwrap());

        let (r1, r2) = std::thread::scope(|s| {
            let r1 = s.spawn(|| rx.recv());
            let r2 = s.spawn(|| rx.recv());

            (r1.join().unwrap(), r2.join().unwrap())
        });

        // 只有一个线程能成功接收数据
        assert!(r1.is_some() || r2.is_some());
        assert!(r1.is_none() || r2.is_none());
    }

    #[test]
    fn test_oneshot_multiple_threads_arc_recv() {
        let (tx, rx) = channel();
        let rx = std::sync::Arc::new(rx);

        let rx1 = rx.clone();
        let rx2 = rx.clone();

        let h1 = std::thread::spawn(move || rx1.recv());
        let h2 = std::thread::spawn(move || rx2.recv());

        std::thread::spawn(move || tx.send(99).unwrap());

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // 只有一个线程能成功接收数据
        assert!(r1.is_some() || r2.is_some());
        assert!(r1.is_none() || r2.is_none());
    }
}

#[cfg(test)]
#[cfg(feature = "async")]
mod async_tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        unsafe fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    fn counting_waker(counter: Arc<AtomicU32>) -> Waker {
        unsafe fn clone(ptr: *const ()) -> RawWaker {
            let arc_ptr = ptr as *const Arc<AtomicU32>;
            // SAFETY: ptr always points to a Box<Arc<AtomicU32>> created by counting_waker.
            let arc = unsafe { (*arc_ptr).clone() };
            arc.fetch_add(0x010, Release);
            let boxed = Box::new(arc);
            RawWaker::new(Box::into_raw(boxed) as *const (), &VTABLE)
        }
        unsafe fn wake(ptr: *const ()) {
            let arc_ptr = ptr as *const Arc<AtomicU32>;
            // SAFETY: ptr is valid and uniquely owned by this wake call.
            unsafe {
                (*arc_ptr).fetch_add(0x001, Release);
                let _ = Box::from_raw(ptr as *mut Arc<AtomicU32>);
            }
        }
        unsafe fn wake_by_ref(ptr: *const ()) {
            let arc_ptr = ptr as *const Arc<AtomicU32>;
            // SAFETY: ptr remains valid; wake_by_ref does not consume ownership.
            unsafe { (*arc_ptr).fetch_add(0x001, Release) };
        }
        unsafe fn drop(ptr: *const ()) {
            // SAFETY: drop consumes the boxed Arc created by counting_waker/clone.
            unsafe {
                let b = Box::from_raw(ptr as *mut Arc<AtomicU32>);
                b.fetch_add(0x100, Release);
            }
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        let boxed = Box::new(counter);
        let ptr = Box::into_raw(boxed) as *const ();
        unsafe { Waker::from_raw(RawWaker::new(ptr, &VTABLE)) }
    }

    #[test]
    fn test_oneshot_async_poll() {
        let (tx, mut rx) = channel();

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let p1 = Pin::new(&mut rx).poll(&mut cx);
        assert_eq!(p1, Poll::Pending);
        let p2 = Pin::new(&mut rx).poll(&mut cx);
        assert_eq!(p2, Poll::Pending);

        //发送后再次轮询应为 Ready(Some)
        tx.send(7).unwrap();
        let p3 = Pin::new(&mut rx).poll(&mut cx);
        assert!(matches!(p3, Poll::Ready(Some(7))));

        //重复轮询应为 Ready(None)
        let p4 = Pin::new(&mut rx).poll(&mut cx);
        assert_eq!(p4, Poll::Ready(None));
    }

    #[test]
    fn test_scheduler_thread_switch_and_waker_replace() {
        for _ in 0..1000 {
            let (tx, mut rx) = channel::<usize>();

            let wake_a = Arc::new(AtomicU32::new(0));
            let wake_b = Arc::new(AtomicU32::new(0));
            let wake_b_thread = wake_b.clone();

            // Scheduler A: poll once and store waker A.
            let waker_a = counting_waker(wake_a.clone());
            let mut cx_a = Context::from_waker(&waker_a);
            assert_eq!(Pin::new(&mut rx).poll(&mut cx_a), Poll::Pending);

            // synchronize `send` and `poll`, manually race
            let sync0 = Arc::new(AtomicU32::new(2));
            let sync1 = sync0.clone();
            // Scheduler B: move receiver to another thread to simulate scheduler switch.
            let handle = std::thread::spawn(move || {
                let waker_b = counting_waker(wake_b_thread.clone());
                let mut cx_b = Context::from_waker(&waker_b);
                sync1.fetch_sub(1, Release);
                while sync1.load(Acquire) != 0 {
                    std::hint::spin_loop();
                }
                // Second poll should replace waker with B and still be pending.
                // Or sender could have sent already, return ready.
                match Pin::new(&mut rx).poll(&mut cx_b) {
                    Poll::Ready(r) => return Poll::Ready(r),
                    Poll::Pending => {}
                }

                // Wait for wake signal, then poll to completion.
                let start = std::time::Instant::now();
                loop {
                    if wake_b_thread.load(Acquire) & 0x00F > 0 {
                        break;
                    }
                    if start.elapsed() > std::time::Duration::from_secs(3) {
                        panic!("Timeout waiting for waker B");
                    }
                    std::hint::spin_loop();
                }

                Pin::new(&mut rx).poll(&mut cx_b)
            });

            // Ensure scheduler B has installed its waker before sending.
            sync0.fetch_sub(1, Release);
            while sync0.load(Acquire) != 0 {
                std::hint::spin_loop();
            }
            tx.send(7).unwrap();

            let result = handle.join().unwrap();
            assert_eq!(result, Poll::Ready(Some(7)));
            let status_a = wake_a.load(Acquire);
            // clone once
            assert_eq!(status_a & 0x0F0, 0x010);
            let woke_a_when_sending = status_a & 0x00F > 0;
            // drop once if wake_a is not woken
            assert_eq!(status_a & 0xF00, if woke_a_when_sending { 0 } else { 0x100 });
            // wake once if wake_a is woken
            assert_eq!(status_a & 0x00F, if woke_a_when_sending { 1 } else { 0 });

            let status_b = wake_b.load(Acquire);
            // clone once if wake_a is not woken
            assert_eq!(status_b & 0x0F0, if woke_a_when_sending { 0 } else { 0x010 });
            let woke_b_when_sending = status_b & 0x00F > 0;
            // wake once, if wake_b is woken
            assert_eq!(status_b & 0x00F, if woke_b_when_sending { 1 } else { 0 });
            // only at most one waker can be woken when sending
            assert_eq!(woke_a_when_sending && woke_b_when_sending, false);
            // drop once at least(by cx), twice if neither is woken(polling interrupted by sender)
            assert_eq!(status_b & 0xF00, if !woke_a_when_sending && !woke_b_when_sending {0x200} else {0x100});
        }
    }
}

