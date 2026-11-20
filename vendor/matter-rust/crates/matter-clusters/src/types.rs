//! Hand-written support types referenced by generated cluster code.

/// A value that may be explicitly null on the wire (Matter quality `X`).
///
/// Distinct from [`Option`]: `Option<T>` models an **optional** element
/// (its tag is absent entirely), whereas `Nullable<T>` models a **present**
/// element whose TLV value is the null type. A field that is both optional
/// and nullable is `Option<Nullable<T>>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nullable<T> {
    /// The wire carried an explicit TLV null.
    Null,
    /// The wire carried a concrete value.
    Value(T),
}

impl<T> Nullable<T> {
    /// Returns the contained value, or `None` if null.
    pub fn value(self) -> Option<T> {
        match self {
            Nullable::Null => None,
            Nullable::Value(v) => Some(v),
        }
    }

    /// Returns `true` if this is [`Nullable::Null`].
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Nullable::Null)
    }
}

impl<T> From<Option<T>> for Nullable<T> {
    fn from(o: Option<T>) -> Self {
        match o {
            Some(v) => Nullable::Value(v),
            None => Nullable::Null,
        }
    }
}

/// A secret passed to a request encoder: a PIN, a passcode verifier, a key,
/// a Thread operational dataset or Wi-Fi credentials.
///
/// `Debug` prints the quoted placeholder `"<redacted>"` instead of the value,
/// so an argument logged with `{:?}` does not leak, and there is no `Display`.
/// Read the value with [`expose`](Self::expose) or take it back with
/// [`into_inner`](Self::into_inner). Build one with [`new`](Self::new) or
/// `From`/`Into`:
///
/// ```
/// use matter_clusters::types::Secret;
///
/// let pin: Secret<Vec<u8>> = b"1234".to_vec().into();
/// assert_eq!(format!("{pin:?}"), r#""<redacted>""#);
/// assert_eq!(pin.expose(), b"1234");
/// ```
///
/// This is a logging guard, not memory protection: the value is not zeroed
/// on drop, `Clone` copies it, and `==` compares it in variable time. The
/// payload an encoder returns carries the secret in clear, so do not log it
/// either.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value.
    #[must_use]
    pub const fn expose(&self) -> &T {
        &self.0
    }

    /// Unwrap, returning the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> core::fmt::Debug for Secret<T> {
    /// Prints `"<redacted>"`, never the value.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt("<redacted>", f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_never_prints_the_value() {
        let pin = Secret::new("12345678".to_string());
        assert_eq!(format!("{pin:?}"), "\"<redacted>\"");
        assert_eq!(format!("{pin:#?}"), "\"<redacted>\"");
        assert_eq!(
            format!("{:?}", Some(Secret::new(vec![1u8, 2]))),
            "Some(\"<redacted>\")"
        );
        assert!(!format!("{pin:?}").contains("1234"));
    }

    #[test]
    fn secret_round_trips_its_value() {
        let key = Secret::from(vec![0xAAu8; 16]);
        assert_eq!(key.expose(), &vec![0xAAu8; 16]);
        assert_eq!(key.clone(), key);
        assert_eq!(key.into_inner(), vec![0xAAu8; 16]);
        assert_ne!(Secret::new(1u8), Secret::new(2u8));
    }

    #[test]
    fn value_and_is_null() {
        assert_eq!(Nullable::Value(7u8).value(), Some(7));
        assert!(Nullable::<u8>::Null.value().is_none());
        assert!(Nullable::<u8>::Null.is_null());
        assert!(!Nullable::Value(7u8).is_null());
    }

    #[test]
    fn from_option() {
        assert_eq!(Nullable::from(Some(3u8)), Nullable::Value(3));
        assert_eq!(Nullable::<u8>::from(None), Nullable::Null);
    }
}
