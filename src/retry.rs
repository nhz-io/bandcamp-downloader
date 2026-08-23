use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::future::Future;
use tokio::time::{sleep, Duration};

/// A failure that a subsequent attempt cannot correct, for example an item that Bandcamp removed
#[derive(Debug)]
pub struct TerminalError {
    message: String,
    /// True if Bandcamp supplies a new link when a person requests one.
    /// The tool cannot correct this, but the operator can.
    needs_relink: bool,
}

impl TerminalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), needs_relink: false }
    }

    /// Bandcamp has the item, but the link needs a replacement. The download page
    /// asks for an email address and sends a new link to it.
    pub fn needs_relink(message: impl Into<String>) -> Self {
        Self { message: message.into(), needs_relink: true }
    }
}

impl Display for TerminalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.message)
    }
}

impl Error for TerminalError {}

/// True if the tool can never download the item, and not only at this time
pub fn is_terminal(error: &Box<dyn Error>) -> bool {
    error.downcast_ref::<TerminalError>().is_some()
}

/// True if the tool can download the item after a person requests a new link
pub fn needs_relink(error: &Box<dyn Error>) -> bool {
    error.downcast_ref::<TerminalError>().map(|e| e.needs_relink).unwrap_or(false)
}

/// The number of attempts before the tool stops
const MAX_ATTEMPTS: usize = 5;

/// The delay before the first new attempt. The tool doubles it after each failure.
const BASE_DELAY: Duration = Duration::from_secs(1);

/// Do an operation. If it fails, do it again after an increasing delay.
///
/// Bandcamp sometimes answers a correct download request without the headers that
/// describe the file. Thus a failure is usually temporary and the tool must try again.
pub async fn with_retry<T, F, Fut>(what: &str, mut operation: F) -> Result<T, Box<dyn Error>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output=Result<T, Box<dyn Error>>>,
{
    let mut delay = BASE_DELAY;

    for attempt in 1..=MAX_ATTEMPTS {
        match operation().await {
            Ok(value) => return Ok(value),
            // A new attempt for a removed item only adds delay before the same failure
            Err(e) if is_terminal(&e) => return Err(e),
            Err(e) if attempt < MAX_ATTEMPTS => {
                eprintln!(
                    "{} failed (attempt {} of {}): {}. Try again in {} seconds.",
                    what, attempt, MAX_ATTEMPTS, e, delay.as_secs()
                );
                sleep(delay).await;
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }

    unreachable!("the loop returns on the final attempt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test(start_paused = true)]
    async fn recovers_once_the_operation_stops_failing() {
        let attempts = Cell::new(0);
        let counter = &attempts;

        let result = with_retry("test", move || async move {
            counter.set(counter.get() + 1);

            match counter.get() {
                3 => Ok("downloaded"),
                _ => Err("no Content-Disposition header".into()),
            }
        }).await;

        assert_eq!(result.unwrap(), "downloaded");
        assert_eq!(attempts.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_last_attempt() {
        let attempts = Cell::new(0);
        let counter = &attempts;

        let result: Result<(), Box<dyn Error>> = with_retry("test", move || async move {
            counter.set(counter.get() + 1);
            Err("always fails".into())
        }).await;

        assert!(result.is_err());
        assert_eq!(attempts.get(), MAX_ATTEMPTS);
    }
}
