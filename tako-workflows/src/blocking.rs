//! Bound synchronous storage operations before dispatching them off the runtime.

use tokio::sync::Semaphore;

// A database outage must not consume Tokio's entire blocking-thread pool.
static STORAGE_OPERATIONS: Semaphore = Semaphore::const_new(32);

pub(crate) async fn run<T: Send + 'static>(
    operation: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    let permit = STORAGE_OPERATIONS
        .acquire()
        .await
        .expect("storage semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
}
