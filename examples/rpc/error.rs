use std::{borrow::Cow, fmt::Display, num::TryFromIntError};

#[derive(Debug)]
pub struct Error {
    message: Cow<'static, str>,
}

impl Error {
    pub const fn new(message: String) -> Self {
        Self {
            message: Cow::Owned(message),
        }
    }

    pub const fn new_static(message: &'static str) -> Self {
        Self {
            message: Cow::Borrowed(message),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<String> for Error {
    fn from(value: String) -> Self {
        Error::new(value)
    }
}

impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Error::new_static(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<TryFromIntError> for Error {
    fn from(value: TryFromIntError) -> Self {
        Self::new(value.to_string())
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Error").field(&self.message).finish()
    }
}
