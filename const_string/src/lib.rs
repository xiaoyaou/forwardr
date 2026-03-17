//! 简化标准库中[`String`]类型的缓冲区长度，创建定长的字符串类型[`ConstString`]，创建完成后不可修改（增删）。
//! 必要时，通过[`ConstString::as_mut_str`]获取可变字符串切片引用，可以进行等长字符替换，但需要调用方自行保证字符串格式有效性。

mod impls;

use std::alloc::{handle_alloc_error, Layout};
use std::mem::MaybeUninit;

/// 栈上字符串的最大长度
const STACK_LEN_MAX: usize = 15;

const PTR_ALIGN: usize = 2;

/// 定长字符串类型，长度较小时直接在栈上初始化，否则在堆上分配新内存
///
/// # 样例：栈上分配
/// ```
/// use const_string::ConstString;
/// let cs = ConstString::new("Short One");
/// assert_eq!(cs.as_str(), "Short One");
/// ```
///
/// # 样例：堆上分配
/// ```
/// use const_string::ConstString;
/// let cs = ConstString::new("This is a longer string!");
/// // 直接转换为String实例，无需重新堆分配
/// let string = cs.to_string();
/// assert_eq!(string, "This is a longer string!");
/// assert_eq!(string.capacity(), string.len());
/// ```
#[doc(alias = "CS")]
#[repr(transparent)]
pub struct ConstString {
    inner: HeapStackStr,
}

impl ConstString {
    pub fn new(val: &str) -> Self {
        ConstString {
            inner: HeapStackStr::new(val),
        }
    }
}

impl ConstString {
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }
    pub fn as_mut_str(&mut self) -> &mut str {
        self.inner.as_mut_str()
    }
}

/// 内部字符串结构，根据指针标记技术来区分栈/堆分配，对外隐藏了实现细节
union HeapStackStr {
    heap: HeapStr,
    stack: StackStr,
}

impl HeapStackStr {
    fn new(val: &str) -> Self {
        if val.len() <= STACK_LEN_MAX {
            Self {
                stack: StackStr::new(val),
            }
        } else {
            Self {
                // SAFETY: val自身保证了指针参数有效
                heap: HeapStr::new(val),
            }
        }
    }

    fn is_stack(&self) -> bool {
        // 利用指针标记技术检查 stack tag
        //
        // 区分栈/堆有两种方式：
        // 1. 利用指针低位对齐留白来标记额外消息
        // 2. 全0初始化，检查低位对齐留白和非空指针 `(unsafe { self.stack.len }) & 0xF != 0 || unsafe { self.ptr.is_null() }`
        //
        // 此处之所以选择方案1，是因为此方案没有额外的分支跳转，且优化后总体指令数更少；
        // 作为对比，方案2因其引入了更多条件分支，破坏了指令流水线，增加CPU分支预测失败风险。
        //
        // 另一方面，基于方案1的`len()`方法需要额外的移位指令，故显著慢于方案2，但是在其他方法上的benchmark结果显示，整体不会更慢
        (unsafe { self.stack.len }) & 0x1 != 0
    }
}

impl HeapStackStr {
    fn len(&self) -> usize {
        if self.is_stack() {
            unsafe { self.stack.len() }
        } else {
            unsafe { self.heap.len() }
        }
    }

    fn as_str(&self) -> &str {
        if self.is_stack() {
            unsafe { self.stack.as_str() }
        } else {
            unsafe { self.heap.as_str() }
        }
    }

    fn as_mut_str(&mut self) -> &mut str {
        if self.is_stack() {
            unsafe { self.stack.as_mut_str() }
        } else {
            unsafe { self.heap.as_mut_str() }
        }
    }
}

/// 由于`union`类型的`Copy`限制，[`HeapStr`]无法实现`Drop` trait。
/// 故而由上级类型[`HeapStackStr`]负责释放堆内存。
impl Drop for HeapStackStr {
    fn drop(&mut self) {
        if self.is_stack() {
            return;
        }
        unsafe {
            std::alloc::dealloc(
                self.heap.ptr,
                Layout::from_size_align_unchecked(self.heap.len(), PTR_ALIGN),
            )
        };
    }
}

/// 栈上字符串，最大长度为[15](`STACK_LEN_MAX`)
#[derive(Clone, Copy)]
#[repr(C)]
struct StackStr {
    #[cfg(target_endian = "little")]
    len: u8,
    str: MaybeUninit<[u8; STACK_LEN_MAX]>,
    #[cfg(target_endian = "big")]
    len: u8,
}

/// 堆上字符串，分配等长字符串空间
#[derive(Clone, Copy)]
#[repr(C)]
struct HeapStr {
    #[cfg(target_endian = "little")]
    ptr: *mut u8,
    len: usize,
    #[cfg(target_endian = "big")]
    ptr: *mut u8,
}

impl StackStr {
    fn new(val: &str) -> Self {
        let mut stack: StackStr = StackStr {
            len: ((val.len() as u8) << 1) | 0x1, // tag stack
            str: MaybeUninit::uninit(),
        };
        let src = val.as_ptr();
        let dst = stack.str.as_mut_ptr() as *mut u8;
        unsafe {
            // SAFETY: len由调用方保证不会溢出
            std::ptr::copy_nonoverlapping(src, dst, val.len())
        };
        stack
    }

    fn len(&self) -> usize {
        (self.len >> 1) as usize
    }

    fn as_str(&self) -> &str {
        unsafe {
            str::from_utf8_unchecked(std::slice::from_raw_parts(
                self.str.as_ptr() as *const u8,
                self.len(),
            ))
        }
    }
    fn as_mut_str(&mut self) -> &mut str {
        unsafe {
            str::from_utf8_unchecked_mut(std::slice::from_raw_parts_mut(
                self.str.as_mut_ptr() as *mut u8,
                self.len(),
            ))
        }
    }
}

impl HeapStr {
    fn new(val: &str) -> Self {
        let heap = HeapStr {
            // SAFETY: len由调用方保证大小
            ptr: unsafe {
                let layout = Layout::from_size_align_unchecked(val.len(), PTR_ALIGN);
                let ptr = std::alloc::alloc(layout);
                if ptr.is_null() {
                    handle_alloc_error(layout)
                }
                ptr
            },
            len: val.len(),
        };
        unsafe { std::ptr::copy_nonoverlapping(val.as_ptr(), heap.ptr, val.len()) };
        heap
    }
    fn len(&self) -> usize {
        self.len
    }

    fn as_str(&self) -> &str {
        unsafe { str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, self.len())) }
    }

    fn as_mut_str(&mut self) -> &mut str {
        unsafe {
            str::from_utf8_unchecked_mut(std::slice::from_raw_parts_mut(self.ptr, self.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_any_len_as_str() {
        macro_rules! as_str {
            ($str: literal) => {
                assert_eq!(ConstString::new($str).as_str(), $str);
            };
        }
        as_str!("");
        as_str!("1");
        as_str!("12");
        as_str!("123");
        as_str!("1234");
        as_str!("12345");
        as_str!("123456");
        as_str!("1234567");
        as_str!("12345678");
        as_str!("123456789");
        as_str!("1234567890");
        as_str!("1234567890A");
        as_str!("1234567890AB");
        as_str!("1234567890ABC");
        as_str!("1234567890ABCD");
        as_str!("1234567890ABCDE");
        as_str!("1234567890ABCDEF");
        as_str!("1234567890ABCDEFX");
        as_str!("1234567890ABCDEFXY");
        as_str!("1234567890ABCDEFXYZ");
    }

    #[test]
    fn test_any_len_into_string() {
        macro_rules! into_string {
            ($str: literal) => {
                assert_eq!(String::from(ConstString::new($str)), $str);
            };
        }
        into_string!("");
        into_string!("1");
        into_string!("12");
        into_string!("123");
        into_string!("1234");
        into_string!("12345");
        into_string!("123456");
        into_string!("1234567");
        into_string!("12345678");
        into_string!("123456789");
        into_string!("1234567890");
        into_string!("1234567890A");
        into_string!("1234567890AB");
        into_string!("1234567890ABC");
        into_string!("1234567890ABCD");
        into_string!("1234567890ABCDE");
        into_string!("1234567890ABCDEF");
        into_string!("1234567890ABCDEFX");
        into_string!("1234567890ABCDEFXY");
        into_string!("1234567890ABCDEFXYZ");
    }

    #[test]
    fn test_short_const_str() {
        let cs = ConstString::new("012345678912345");

        println!("cst: {:p}", &cs);

        let slice = cs.as_str();

        println!("ptr: {:p}, len: {}", slice.as_ptr(), slice.len());

        println!("str: {}", cs.as_str());
    }

    #[test]
    fn test_long_const_str() {
        let cs = ConstString::new("0123456789123456");

        println!("cst: {:p}", &cs);

        let slice = cs.as_str();

        println!("ptr: {:p}, len: {}", slice.as_ptr(), slice.len());

        println!("str: {}", cs.as_str());
    }
}
