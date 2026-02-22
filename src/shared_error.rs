use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SharedError(pub Arc<anyhow::Error>);

impl fmt::Display for SharedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl From<anyhow::Error> for SharedError {
    fn from(err: anyhow::Error) -> Self {
        SharedError(Arc::new(err))
    }
}
