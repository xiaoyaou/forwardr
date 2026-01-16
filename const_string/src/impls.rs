use std::{
    borrow::{Borrow, BorrowMut},
    convert::Infallible,
    fmt::{Debug, Display},
    hash::Hash,
    ops::{Deref, DerefMut},
    str::FromStr,
};

use crate::{ConstString, HeapStackStr, HeapStr};

impl Deref for ConstString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl DerefMut for ConstString {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_str()
    }
}

impl AsRef<str> for ConstString {
    fn as_ref(&self) -> &str {
        self
    }
}

impl AsMut<str> for ConstString {
    fn as_mut(&mut self) -> &mut str {
        self
    }
}

impl Borrow<str> for ConstString {
    fn borrow(&self) -> &str {
        self
    }
}

impl BorrowMut<str> for ConstString {
    fn borrow_mut(&mut self) -> &mut str {
        self
    }
}

impl Default for ConstString {
    fn default() -> Self {
        Self::new("")
    }
}

impl Debug for ConstString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self.as_str(), f)
    }
}

impl Display for ConstString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self.as_str(), f)
    }
}

impl From<&str> for ConstString {
    fn from(value: &str) -> Self {
        ConstString::new(value)
    }
}

impl From<String> for ConstString {
    fn from(value: String) -> Self {
        ConstString::new(&value)
    }
}

impl FromStr for ConstString {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ConstString::from(s))
    }
}

impl From<ConstString> for String {
    fn from(value: ConstString) -> Self {
        value.into_string()
    }
}

impl Hash for ConstString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl PartialEq for ConstString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl Eq for ConstString {}

impl PartialOrd for ConstString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl Ord for ConstString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Clone for ConstString {
    fn clone(&self) -> Self {
        if self.inner.is_stack() {
            ConstString {
                inner: HeapStackStr {
                    // SAFETY: 栈上字符串可直接copy
                    stack: unsafe { self.inner.stack },
                },
            }
        } else {
            ConstString {
                inner: HeapStackStr {
                    // SAFETY: 堆上字符串可直接复刻创建
                    heap: unsafe { HeapStr::new(self.inner.heap.ptr, self.inner.heap.len) },
                },
            }
        }
    }
}
