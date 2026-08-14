//! `flacenc::error`: verification wrappers and the error types the encoder
//! hands back.

use std::fmt;
use std::marker::PhantomData;

use crate::bitsink::BitSink;

/// A value that has passed [`Verify::verify`].
///
/// Derefs to the value, which is how callers read `config.block_size` off a
/// verified config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified<T>(T);

impl<T> std::ops::Deref for Verified<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Configuration structs that can check their own consistency.
pub trait Verify: Sized {
    /// Check every field; [`VerifyError`] names the first bad one.
    fn verify(&self) -> Result<(), VerifyError>;

    /// Wrap into [`Verified`], or hand the value back with the reason.
    fn into_verified(self) -> Result<Verified<Self>, (Self, VerifyError)> {
        match self.verify() {
            Ok(()) => Ok(Verified(self)),
            Err(e) => Err((self, e)),
        }
    }
}

/// A configuration field that is out of range, with the path to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    components: Vec<String>,
    reason: String,
}

impl VerifyError {
    /// An error for an invalid field `component`.
    pub fn new(component: &str, reason: &str) -> Self {
        VerifyError {
            components: vec![component.to_owned()],
            reason: reason.to_owned(),
        }
    }

    /// Prefix the field path with an enclosing struct's field name.
    pub fn within(mut self, component: &str) -> Self {
        self.components.insert(0, component.to_owned());
        self
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "verification error: `{}` is not valid. reason: {}",
            self.components.join("."),
            self.reason
        )
    }
}

impl std::error::Error for VerifyError {}

/// Encoding failed: either the source or the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub(crate) String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

/// Writing a component to a [`BitSink`] failed.
///
/// Generic over the sink, as the incumbent is, so `write` signatures match; our
/// only sink is infallible, so this is never constructed in practice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputError<S: BitSink> {
    reason: String,
    sink: PhantomData<fn() -> S>,
}

impl<S: BitSink> OutputError<S> {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        OutputError {
            reason: reason.into(),
            sink: PhantomData,
        }
    }
}

impl<S: BitSink> fmt::Display for OutputError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl<S: BitSink + fmt::Debug> std::error::Error for OutputError<S> {}
