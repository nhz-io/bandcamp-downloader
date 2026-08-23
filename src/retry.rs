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

/// Bandcamp refused the request. The item is good and the tool must wait for it.
#[derive(Debug)]
pub struct ThrottledError(pub String);

impl Display for ThrottledError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "{}", self.0)
    }
}

impl Error for ThrottledError {}

/// True if Bandcamp refused the request, and did not fail it
pub fn is_throttled(error: &Box<dyn Error>) -> bool {
    error.downcast_ref::<ThrottledError>().is_some()
}

/// The number of attempts before the tool stops
const MAX_ATTEMPTS: usize = 5;

/// The delay before the first new attempt. The tool doubles it after each failure.
const BASE_DELAY: Duration = Duration::from_secs(1);

/// How many times to wait for Bandcamp to accept the requests again.
///
/// A refusal is not a failure of the item. It says that the tool must wait. Thus a
/// refusal does not use an attempt: the attempts are for an item that is truly bad,
/// and an item that is good must not be lost because the tool ran out of them while
/// it learned how long to wait.
const MAX_WAITS: usize = 15;

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
    let mut attempt = 1;
    let mut waits = 0;

    loop {
        match operation().await {
            Ok(value) => return Ok(value),

            // A new attempt for a removed item only adds delay before the same failure
            Err(e) if is_terminal(&e) => return Err(e),

            // Bandcamp refused. The pace already grew, and the next request waits for
            // it, thus this only counts the waits and does not add a delay of its own.
            Err(e) if is_throttled(&e) && waits < MAX_WAITS => {
                waits += 1;

                eprintln!("{} is waiting for Bandcamp ({} of {}): {}", what, waits, MAX_WAITS, e);
            }

            Err(e) if attempt < MAX_ATTEMPTS => {
                eprintln!(
                    "{} failed (attempt {} of {}): {}. Try again in {} seconds.",
                    what, attempt, MAX_ATTEMPTS, e, delay.as_secs()
                );

                sleep(delay).await;

                delay *= 2;
                attempt += 1;
            }

            Err(e) => return Err(e),
        }
    }
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
    async fn a_refusal_does_not_use_an_attempt() {
        let tries = Cell::new(0);
        let counter = &tries;

        // More refusals than there are attempts. The item is good, thus it must
        // survive them and not be lost while the pace learns how long to wait.
        let refusals = MAX_ATTEMPTS + 3;

        let result = with_retry("test", move || async move {
            counter.set(counter.get() + 1);

            match counter.get() > refusals {
                true => Ok("downloaded"),
                _ => Err(ThrottledError("Bandcamp sent text/html".into()).into()),
            }
        }).await;

        assert_eq!(result.unwrap(), "downloaded");
        assert_eq!(tries.get(), refusals + 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_waiting_for_bandcamp_in_the_end() {
        let tries = Cell::new(0);
        let counter = &tries;

        let result: Result<(), Box<dyn Error>> = with_retry("test", move || async move {
            counter.set(counter.get() + 1);
            Err(ThrottledError("Bandcamp sent text/html".into()).into())
        }).await;

        assert!(result.is_err());
        // The waits, and then the attempts that follow them
        assert_eq!(tries.get(), MAX_WAITS + MAX_ATTEMPTS);
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
