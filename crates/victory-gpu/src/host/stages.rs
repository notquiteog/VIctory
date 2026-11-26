//! The line in two stages (`VICTORY_LINE_STAGES`, on by default; `=0`: one
//! thread does it all, as before).
//!
//! The submitting thread (stage A) prepares a submission: asks the write
//! watch, refreshes host copies, puts the title's bytes in under flights'
//! writes, plans the steps done on the card, takes what goes to copies in the
//! card's memory and sends that. What is left once that is done goes to a
//! thread of its own (stage B), in the order handed over, and stage A goes on
//! with the next submission:
//!
//! 1. the descriptors of steps done on the card ("the slots");
//! 2. the heaps' records ([`crate::descriptor::begin_sync_heaps`]) and the
//!    slots that waited for a texture ([`crate::descriptor::make_waiting`]);
//! 3. the lists given to the queue, and the fence signalled.
//!
//! What holds it together:
//!
//! - One queue, one thread taking from it: the card gets the submissions'
//!   lists in the order stage A handed them over, and each submission's
//!   descriptors are written before its lists go and after those of the one
//!   before it. Submission N+1's heaps' records are therefore made after N's
//!   slots, as when one thread did both.
//! - Whatever else gives the queue work, or signals the fence the line's
//!   flights are numbered by, first [`drain`]s: everything handed over is on
//!   the queue by then. Stage A does so itself before it sends title bytes to
//!   copies in the card's memory, so those never pass a submission before
//!   them.
//! - A flight is noted in the air when it is handed over, before its lists
//!   are on the queue: a wait for its fence waits longer, never too little.
//!
//! Why: the submitting thread is the frame's bottleneck (it worked 50 ms a
//! frame of 50 ms in run son2, 2026-11-26), and the work moved here is some
//! 4 to 9 ms of it that nothing it does next reads.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};

/// Work for stage B.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// A thread that does jobs in the order handed over, and counts them.
pub(crate) struct Stage {
    state: Mutex<State>,
    /// Told when a job comes.
    more: Condvar,
    /// Told when a job is done.
    done: Condvar,
}

#[derive(Default)]
struct State {
    jobs: VecDeque<Job>,
    /// Jobs handed over, and done, since the stage began.
    handed: u64,
    finished: u64,
}

impl Stage {
    /// A stage and its thread, named `name`.
    pub(crate) fn start(name: &str, on_start: fn()) -> &'static Stage {
        let stage: &'static Stage = Box::leak(Box::new(Stage {
            state: Mutex::new(State::default()),
            more: Condvar::new(),
            done: Condvar::new(),
        }));
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                on_start();
                stage.run();
            })
            .expect("a thread for the line's second stage");
        stage
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(&self) {
        loop {
            let job = {
                let mut state = self.state();
                loop {
                    if let Some(job) = state.jobs.pop_front() {
                        break job;
                    }
                    state = self.more.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            let span = crate::timeline::span("the line's second stage");
            #[cfg(feature = "diagnostics")]
            let began = std::time::Instant::now();
            // A job that panics is counted done all the same: what waits for
            // it is not held for ever (the panic is the host's `fail`, which
            // ends the process, or a bug that the log shows).
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            #[cfg(feature = "diagnostics")]
            WORKED.fetch_add(began.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
            drop(span);
            let mut state = self.state();
            state.finished += 1;
            drop(state);
            self.done.notify_all();
        }
    }

    /// Hands `job` over; its number.
    pub(crate) fn hand(&self, job: Job) -> u64 {
        let mut state = self.state();
        state.jobs.push_back(job);
        state.handed += 1;
        let number = state.handed;
        drop(state);
        self.more.notify_one();
        number
    }

    /// Waits until job `number` (and every one before it) is done.
    pub(crate) fn wait(&self, number: u64) {
        let mut state = self.state();
        while state.finished < number {
            state = self.done.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Waits until everything handed over so far is done.
    pub(crate) fn drain(&self) {
        let handed = self.state().handed;
        self.wait(handed);
    }

    /// Whether nothing handed over is left to do.
    #[cfg(test)]
    fn idle(&self) -> bool {
        let state = self.state();
        state.finished == state.handed
    }
}

/// Whether the line works in two stages (`VICTORY_LINE_STAGES=0`: no).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_LINE_STAGES").is_none_or(|v| v != "0");
        tracing::info!(on, "the line in two stages: lists, descriptors and the heaps' records on a thread of their own (VICTORY_LINE_STAGES)");
        on
    })
}

/// Stage B of the line, if [`on`].
fn second() -> Option<&'static Stage> {
    static STAGE: OnceLock<Option<&'static Stage>> = OnceLock::new();
    *STAGE.get_or_init(|| {
        on().then(|| {
            Stage::start("victory-send", || {
                crate::cores::keep_helper_on_fast_cores("victory-send");
            })
        })
    })
}

/// Hands `job` to stage B, or does it now without one.
pub(crate) fn hand(job: Job) {
    match second() {
        Some(stage) => {
            stage.hand(job);
        }
        None => job(),
    }
}

/// Waits until everything handed to stage B is done: its lists are on the
/// queue and its fence signal behind them.
pub(crate) fn drain() {
    if !on() {
        return;
    }
    if let Some(stage) = second() {
        let _waiting = crate::timeline::span("waiting for the line's second stage");
        #[cfg(feature = "diagnostics")]
        let began = std::time::Instant::now();
        stage.drain();
        #[cfg(feature = "diagnostics")]
        WAITED.fetch_add(began.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Since the last frame line: microseconds stage B worked, and microseconds
/// others waited for it ([`drain`]).
#[cfg(feature = "diagnostics")]
pub(crate) static WORKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "diagnostics")]
pub(crate) static WAITED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// For the frame line: the two numbers above, taken, per frame.
#[cfg(feature = "diagnostics")]
pub(crate) fn log(frames: f64) {
    use std::sync::atomic::Ordering::Relaxed;
    if !on() || frames <= 0.0 {
        return;
    }
    let worked = WORKED.swap(0, Relaxed) as f64 / 1000.0 / frames;
    let waited = WAITED.swap(0, Relaxed) as f64 / 1000.0 / frames;
    tracing::info!("the line's second stage per frame: worked {worked:.1} ms; waited for, by the first and others, {waited:.1} ms");
}

#[cfg(test)]
mod tests {
    use super::Stage;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn stage() -> &'static Stage {
        Stage::start("victory-send-test", || {})
    }

    /// Jobs are done one at a time in the order handed over, whatever each
    /// takes: the card gets submissions in title order, and a submission's
    /// descriptors come after the one before it's.
    #[test]
    fn jobs_run_in_order_handed() {
        let stage = stage();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..40u64 {
            let seen = seen.clone();
            stage.hand(Box::new(move || {
                // The earlier ones take longer: out of order they would end
                // later.
                std::thread::sleep(Duration::from_micros((40 - i) * 50));
                seen.lock().unwrap().push(i);
            }));
        }
        stage.drain();
        assert_eq!(*seen.lock().unwrap(), (0..40).collect::<Vec<_>>());
    }

    /// The ordering rule: what comes after a submission's slots in stage A
    /// (the next submission's heaps' records, a send of title bytes, another
    /// queue's use) waits for them. A drain that did not wait would let it
    /// see the slot unwritten.
    #[test]
    fn drain_waits_for_the_slots() {
        let stage = stage();
        let slot = Arc::new(Mutex::new(0u64));
        for n in 1..=3u64 {
            let slot = slot.clone();
            stage.hand(Box::new(move || {
                std::thread::sleep(Duration::from_millis(30));
                *slot.lock().unwrap() = n;
            }));
        }
        stage.drain();
        assert!(stage.idle());
        assert_eq!(*slot.lock().unwrap(), 3, "the next submission's records would be made before the slots");
    }

    /// Waiting for one job waits for those before it, not for those after.
    #[test]
    fn wait_for_a_number() {
        let stage = stage();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (o1, o2) = (order.clone(), order.clone());
        let first = stage.hand(Box::new(move || {
            std::thread::sleep(Duration::from_millis(20));
            o1.lock().unwrap().push(1);
        }));
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let g = gate.clone();
        stage.hand(Box::new(move || {
            let _g = g.lock().unwrap();
            o2.lock().unwrap().push(2);
        }));
        stage.wait(first);
        assert_eq!(*order.lock().unwrap(), vec![1]);
        drop(held);
        stage.drain();
        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
    }
}
