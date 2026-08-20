/// Error type for asset processing failures, shared across all sync modes
/// (`individual`, `raw`, `packed`, `dpi`) so the failure path is consistent.
#[derive(Debug)]
pub struct ProcessingError {
    pub error: anyhow::Error,
}

impl ProcessingError {
    pub fn new(error: anyhow::Error) -> Self {
        Self { error }
    }
}
