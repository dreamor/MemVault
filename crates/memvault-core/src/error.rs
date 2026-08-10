use thiserror::Error;

#[derive(Error, Debug)]
pub enum MemVaultError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, MemVaultError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_error_display() {
        let err = MemVaultError::Storage("disk full".to_string());
        assert_eq!(err.to_string(), "storage error: disk full");
    }

    #[test]
    fn test_not_found_error_display() {
        let err = MemVaultError::NotFound("mem_123".to_string());
        assert_eq!(err.to_string(), "not found: mem_123");
    }

    #[test]
    fn test_invalid_input_error_display() {
        let err = MemVaultError::InvalidInput("empty content".to_string());
        assert_eq!(err.to_string(), "invalid input: empty content");
    }

    #[test]
    fn test_sqlite_from_error() {
        let sqlite_err = rusqlite::Error::InvalidColumnName("bad_col".to_string());
        let err: MemVaultError = sqlite_err.into();
        assert!(err.to_string().contains("sqlite error"));
    }

    #[test]
    fn test_serde_from_error() {
        let serde_err = serde_json::from_str::<String>("invalid json").unwrap_err();
        let err: MemVaultError = serde_err.into();
        assert!(err.to_string().contains("serialization error"));
    }

    #[test]
    fn test_error_is_debug() {
        let err = MemVaultError::NotFound("x".to_string());
        let debug = format!("{:?}", err);
        assert!(debug.contains("NotFound"));
    }

    #[test]
    fn test_result_type_alias() {
        let ok: Result<i32> = Ok(42);
        assert_eq!(ok.unwrap(), 42);

        let err: Result<i32> = Err(MemVaultError::InvalidInput("bad".to_string()));
        assert!(err.is_err());
    }
}
