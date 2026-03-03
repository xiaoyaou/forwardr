/// 基础bit状态位Trait，提供bit掩码值
pub trait Bit {
    fn bit(self) -> u32;
}

/// bit状态转移Trait，提供bit状态的增删查操作
pub trait BitWith<B: Bit> {
    /// 设置指定bit状态，返回新的状态值
    fn with(self, bit: B) -> Self;
    /// 清除指定bit状态，返回新的状态值
    fn without(self, bit: B) -> Self;
    /// 检查指定bit状态是否被设置
    fn is(self, bit: B) -> bool;
}

impl<B: Bit> BitWith<B> for u32 {
    #[inline(always)]
    fn with(self, bit: B) -> u32 {
        self | bit.bit()
    }
    #[inline(always)]
    fn without(self, bit: B) -> u32 {
        self & !bit.bit()
    }
    #[inline(always)]
    fn is(self, bit: B) -> bool {
        self & bit.bit() != 0
    }
}


/// 状态转移宏，使用`+`、`-`精简语义，修改bit状态，获取新的状态值
#[macro_export]
macro_rules! with {
    ($current: expr, + $status: ident $($rest: tt)*) => {
        with!($current.with($status), $($rest)*)
    };
    ($current: expr, - $status: ident $($rest: tt)*) => {
        with!($current.without($status), $($rest)*)
    };
    ($current: expr $(,)?) => {
        $current
    }
}