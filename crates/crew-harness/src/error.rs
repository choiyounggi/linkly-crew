#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("failed to spawn claude process: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("failed to write turn to child stdin: {0}")]
    Write(#[source] std::io::Error),

    #[error("child process exited before a result event arrived")]
    ProcessExited,

    #[error("harness unavailable: {0}")]
    Unavailable(String),
}
