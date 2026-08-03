/// 数据库错误。
#[derive(thiserror::Error, Debug)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("pool error: {0}")]
    Pool(#[from] deadpool_sqlite::PoolError),

    #[error("pool build error: {0}")]
    BuildError(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("{0}")]
    Other(String),
}

impl From<DbError> for String {
    fn from(e: DbError) -> String {
        e.to_string()
    }
}
