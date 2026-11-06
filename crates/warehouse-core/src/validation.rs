use std::fmt;

/// A field the API would reject with HTTP 400.
///
/// This is not `AppError`. The core crate must not depend on axum, and the
/// desktop client checks the same rules before it sends a request. The API
/// still checks again: the client is not the source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

/// Check fields shared by create and update.
///
/// `name` is `&str`: a borrowed view of a string someone else owns.
/// This function only reads it. It does not take ownership, so the caller
/// can still use the string afterwards.
///
/// `Result<(), ValidationError>` means "success with no value, or an error".
/// The empty tuple `()` is Rust's way of saying "nothing to return".
pub fn validate_item(name: &str, quantity: i32) -> Result<(), ValidationError> {
    if name.trim().is_empty() {
        return Err(ValidationError("name must not be empty".to_string()));
    }

    if quantity < 0 {
        return Err(ValidationError(
            "quantity must be greater than or equal to 0".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        let message = validation_message(validate_item("", 1));
        assert!(message.contains("name"));
    }

    #[test]
    fn rejects_whitespace_name() {
        let message = validation_message(validate_item("   ", 1));
        assert!(message.contains("name"));
    }

    #[test]
    fn rejects_negative_quantity() {
        let message = validation_message(validate_item("bolts", -1));
        assert!(message.contains("quantity"));
    }

    #[test]
    fn accepts_zero_quantity() {
        assert!(validate_item("bolts", 0).is_ok());
    }

    #[test]
    fn accepts_normal_item() {
        assert!(validate_item(" bolts ", 3).is_ok());
    }

    fn validation_message(result: Result<(), ValidationError>) -> String {
        match result {
            Err(ValidationError(message)) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }
}
