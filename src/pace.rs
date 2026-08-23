use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// The default time between requests. A download takes minutes, thus this delay does
/// not make the run longer. But it stops the requests from looking automatic.
pub const DEFAULT_INTERVAL_MS: u64 = 150;

static INTERVAL_MS: AtomicU64 = AtomicU64::new(DEFAULT_INTERVAL_MS);

/// The tool adds this to the interval while Bandcamp refuses. It decreases it again after.
static PENALTY_MS: AtomicU64 = AtomicU64::new(0);

/// The number of correct downloads in sequence before the tool decreases the delay
const CLEAN_BEFORE_EASING: u64 = 3;
static CLEAN_RUN: AtomicU64 = AtomicU64::new(0);

/// The increase for each refused request, and the maximum delay
const PENALTY_STEP_MS: u64 = 5_000;
const PENALTY_CAP_MS: u64 = 300_000;

/// Bandcamp refuses the requests. Increase the delay, or use the time that it asks for.
///
/// Bandcamp does not report the refusal. It sends a usual page in place of the file.
/// Thus the tool can only increase the delay until Bandcamp accepts the requests.
pub fn throttled(retry_after: Option<Duration>) {
    let step = retry_after.map(|after| after.as_millis() as u64).unwrap_or(PENALTY_STEP_MS);
    // The delay has a maximum, whatever time Bandcamp asks for
    let slowed = (PENALTY_MS.load(Ordering::Relaxed) + step).min(PENALTY_CAP_MS);

    PENALTY_MS.store(slowed, Ordering::Relaxed);
    CLEAN_RUN.store(0, Ordering::Relaxed);

    eprintln!("Bandcamp refuses the requests. Wait {} seconds between the requests now.", slowed / 1000);
}

/// A download is complete. Decrease the delay after some correct downloads in sequence.
/// Thus one correct response does not remove the full delay.
pub fn succeeded() {
    if PENALTY_MS.load(Ordering::Relaxed) == 0 {
        return;
    }

    if CLEAN_RUN.fetch_add(1, Ordering::Relaxed) + 1 < CLEAN_BEFORE_EASING {
        return;
    }

    let eased = PENALTY_MS.load(Ordering::Relaxed).saturating_sub(PENALTY_STEP_MS);

    PENALTY_MS.store(eased, Ordering::Relaxed);
    CLEAN_RUN.store(0, Ordering::Relaxed);

    match eased {
        0 => eprintln!("Bandcamp accepts the requests again. Use the usual speed."),
        _ => eprintln!("Wait {} seconds between the requests now.", eased / 1000),
    }
}

#[cfg(test)]
fn reset() {
    PENALTY_MS.store(0, Ordering::Relaxed);
    CLEAN_RUN.store(0, Ordering::Relaxed);
}

/// The time of the next permitted request. The tool keeps this and not the time of the
/// last request. Thus each caller waits for the previous one.
static NEXT_ALLOWED: Mutex<Option<Instant>> = Mutex::new(None);

pub fn set_interval(millis: u64) {
    INTERVAL_MS.store(millis, Ordering::Relaxed);
}

/// Wait until sufficient time passes after the previous request.
///
/// The tool calls this before each request. Bandcamp is a shop and not an api. A large
/// collection needs thousands of requests. Thus the tool puts a delay between them.
pub async fn pace() {
    let interval = Duration::from_millis(
        INTERVAL_MS.load(Ordering::Relaxed) + PENALTY_MS.load(Ordering::Relaxed));

    if interval.is_zero() {
        return;
    }

    let wait = {
        let mut next_allowed = NEXT_ALLOWED.lock().unwrap();
        let now = Instant::now();

        let go_at = match *next_allowed {
            Some(allowed) if allowed > now => allowed,
            _ => now,
        };

        // Reserve this time before you release the lock. Thus the next caller waits.
        *next_allowed = Some(go_at + interval);

        go_at.duration_since(now)
    };

    if !wait.is_zero() {
        sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One delay applies to the full run. Thus these tests must not operate together.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn exclusively() -> std::sync::MutexGuard<'static, ()> {
        let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|held| held.into_inner());

        reset();

        guard
    }

    #[test]
    fn slows_down_when_refused_and_eases_back_off() {
        let _exclusive = exclusively();

        throttled(None);
        let first = PENALTY_MS.load(Ordering::Relaxed);
        assert_eq!(first, PENALTY_STEP_MS);

        // A second refusal increases the delay again
        throttled(None);
        assert!(PENALTY_MS.load(Ordering::Relaxed) > first);

        // One correct download does not decrease the delay
        succeeded();
        assert_eq!(PENALTY_MS.load(Ordering::Relaxed), first + PENALTY_STEP_MS);

        // Some correct downloads in sequence decrease it
        succeeded();
        succeeded();
        assert_eq!(PENALTY_MS.load(Ordering::Relaxed), first);
    }

    #[test]
    fn waits_as_long_as_it_was_asked_to() {
        let _exclusive = exclusively();

        throttled(Some(Duration::from_secs(30)));
        assert_eq!(PENALTY_MS.load(Ordering::Relaxed), 30_000);

        // The delay has a maximum, whatever time Bandcamp asks for
        throttled(Some(Duration::from_secs(600)));
        assert_eq!(PENALTY_MS.load(Ordering::Relaxed), PENALTY_CAP_MS);
    }

    #[tokio::test]
    async fn holds_a_gap_between_consecutive_requests() {
        let _exclusive = exclusively();
        set_interval(80);

        // The first request does not wait
        let start = Instant::now();
        pace().await;
        assert!(start.elapsed() < Duration::from_millis(40));

        // The next request waits for the interval
        let start = Instant::now();
        pace().await;
        assert!(start.elapsed() >= Duration::from_millis(60), "expected a gap, waited {:?}", start.elapsed());

        // The third request waits for the second one
        let start = Instant::now();
        pace().await;
        assert!(start.elapsed() >= Duration::from_millis(60));

        set_interval(0);
        let start = Instant::now();
        pace().await;
        // The delay is off, thus no request waits
        assert!(start.elapsed() < Duration::from_millis(40));

        set_interval(DEFAULT_INTERVAL_MS);
    }
}
