//! The title's GPU timestamps, taken on the card (`VICTORY_GPU_STAMPS=1`; off
//! by default: the PC's clock at the landing, as before).
//!
//! Measured (runs nostamp / stamp, the same scene, 63 submissions a frame):
//! median 161 ms a frame without and 161 ms with it; four stamps a frame were
//! taken on the card and all were read. The title did not draw less for being
//! told the card's real time, so it stays off until a scene is found where it
//! does.
//!
//! The title asks for GPU timestamps a few times a frame (a bottom-of-pipe
//! write of the GPU clock, or a timestamp query) and measures the card's
//! time for its frame with them: it sizes what it draws by that time, as it
//! does on the console when a scene is heavy. A timestamp was the PC's clock
//! at the moment the flight that held it landed. All of a frame's stamps
//! land together, so every span the title measured was a few microseconds,
//! "the card did nothing", and the title drew at its full size however long
//! the card really took (run tl2: 62 ms of card time in a 78 ms frame).
//!
//! With the switch a timestamp of the card's own is taken at the stamp's
//! place in the host list (`EndQuery`, and its way into memory the PC reads),
//! and when the list has run the title is given that moment on the clock it
//! was given before: [`crate::queue::TIMESTAMP_FREQUENCY`] ticks a second
//! since the process started. What the title measures is then what the card
//! took between its two stamps, our own lists between them included, as on
//! the console.
//!
//! A/B-able while the title runs: `VICTORY_AB=<s>,VICTORY_GPU_STAMPS`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use super::*;

/// Stamps kept before the first place is used again (the title asks for a
/// handful a frame; a flight lands within a few frames).
const MOST: u32 = 4096;

struct Ring {
    heap: ID3D12QueryHeap,
    buffer: ID3D12Resource,
    /// The mapping of `buffer`: a tick count of the card's for each place.
    back: usize,
    /// The card's ticks a second.
    frequency: u64,
    /// The card's ticks, and the title's GPU clock, at one moment.
    calibrated: (u64, u64),
}

// SAFETY: free-threaded objects, and a mapping the card writes whole words of.
unsafe impl Send for Ring {}
// SAFETY: as above.
unsafe impl Sync for Ring {}

static RING: OnceLock<Option<Ring>> = OnceLock::new();
static NEXT: AtomicU32 = AtomicU32::new(0);
/// Since the last frame-rate line: stamps taken on the card, stamps read,
/// stamps read that the card had not written.
static STATS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Whether the title's GPU timestamps are the card's.
pub(crate) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_GPU_STAMPS") {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GPU_STAMPS").is_some_and(|v| v == "1"))
}

fn ring() -> Option<&'static Ring> {
    RING.get_or_init(|| {
        let host = host()?;
        match host.make_true_stamps() {
            Ok(ring) => {
                tracing::info!(
                    frequency = ring.frequency,
                    "timestamps are taken on the card (VICTORY_GPU_STAMPS=1: the title's; VICTORY_OWN_SPANS=1: around our own lists)"
                );
                Some(ring)
            }
            Err(e) => {
                tracing::warn!(error = %e, "the title's GPU timestamps stay the PC's clock: no timestamp queries");
                None
            }
        }
    })
    .as_ref()
}

/// Takes a timestamp of the card's at this place of the open list `list`:
/// the place to read it from once the list has run ([`read`]). `None`: not
/// asked for, or it cannot be had.
pub(crate) fn stamp(list: &ID3D12GraphicsCommandList) -> Option<u32> {
    if !on() {
        return None;
    }
    let ring = ring()?;
    let at = NEXT.fetch_add(1, Relaxed) % MOST;
    // SAFETY: an open list; a query and eight bytes of the buffer that are
    // this place's alone until the ring comes round. The word is zeroed so
    // that a stamp the card never wrote is told from one it did.
    unsafe {
        (ring.back as *mut u64).add(at as usize).write_volatile(0);
        list.EndQuery(&ring.heap, D3D12_QUERY_TYPE_TIMESTAMP, at);
        list.ResolveQueryData(&ring.heap, D3D12_QUERY_TYPE_TIMESTAMP, at, 1, &ring.buffer, u64::from(at) * 8);
    }
    STATS[0].fetch_add(1, Relaxed);
    Some(at)
}

/// Diagnostics (`VICTORY_OWN_SPANS=1`): the card's time in the lists of ours
/// that go around a submission's (the sends to copies in the card's memory
/// with their hash passes ahead of it, the compare behind it), by stamps at
/// their first and last command.
fn metered() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_OWN_SPANS").is_some_and(|v| v == "1"))
}

/// A stamp at this place of a list of ours, if they are metered.
pub(super) fn mark(list: &ID3D12GraphicsCommandList) -> Option<u32> {
    if !metered() {
        return None;
    }
    let ring = ring()?;
    let at = NEXT.fetch_add(1, Relaxed) % MOST;
    // SAFETY: as in `stamp`.
    unsafe {
        (ring.back as *mut u64).add(at as usize).write_volatile(0);
        list.EndQuery(&ring.heap, D3D12_QUERY_TYPE_TIMESTAMP, at);
        list.ResolveQueryData(&ring.heap, D3D12_QUERY_TYPE_TIMESTAMP, at, 1, &ring.buffer, u64::from(at) * 8);
    }
    Some(at)
}

/// Marks waiting to be read: kind (0: sends and hashes, 1: compare), the two
/// places, when they were recorded.
static MARKED: Mutex<std::collections::VecDeque<(usize, u32, u32, std::time::Instant)>> = Mutex::new(std::collections::VecDeque::new());
/// Since the last frame-rate line, by kind: lists, and the card's ticks in them.
static OWN: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// A list of ours of `kind` has stamps at its two ends. Those recorded a
/// while ago (their lists have run) are read.
pub(super) fn marked(kind: usize, from: Option<u32>, to: Option<u32>) {
    let (Some(from), Some(to)) = (from, to) else {
        return;
    };
    let Some(ring) = RING.get().and_then(|r| r.as_ref()) else {
        return;
    };
    let now = std::time::Instant::now();
    let mut marked = MARKED.lock().unwrap_or_else(|e| e.into_inner());
    marked.push_back((kind, from, to, now));
    while marked.front().is_some_and(|m| (now - m.3).as_millis() > 400) {
        let Some((kind, from, to, _)) = marked.pop_front() else {
            break;
        };
        // SAFETY: inside the mapping; whole words the card wrote.
        let (a, b) = unsafe {
            let back = ring.back as *const u64;
            (back.add(from as usize).read_volatile(), back.add(to as usize).read_volatile())
        };
        if a != 0 && b >= a {
            OWN[2 * kind].fetch_add(1, Relaxed);
            OWN[2 * kind + 1].fetch_add(b - a, Relaxed);
        }
    }
}

/// The card's ticks `card` on the title's GPU clock, by a calibration
/// (`card0` ticks were `title0`) and the card's ticks a second.
fn placed(card: u64, (card0, title0): (u64, u64), frequency: u64) -> u64 {
    let per_second = i128::from(crate::queue::TIMESTAMP_FREQUENCY);
    let passed = (i128::from(card) - i128::from(card0)) * per_second / i128::from(frequency.max(1));
    (i128::from(title0) + passed).max(1) as u64
}

/// The stamp at `at` on the title's GPU clock, once its list has run.
/// `None`: the card did not write it (the caller gives the PC's clock).
pub(crate) fn read(at: u32) -> Option<u64> {
    let ring = RING.get()?.as_ref()?;
    STATS[1].fetch_add(1, Relaxed);
    // SAFETY: inside the mapping; the card wrote a whole word, and its list
    // has run.
    let card = unsafe { (ring.back as *const u64).add((at % MOST) as usize).read_volatile() };
    if card == 0 {
        STATS[2].fetch_add(1, Relaxed);
        return None;
    }
    Some(placed(card, ring.calibrated, ring.frequency))
}

/// For the frame-rate line.
pub(super) fn line(frames: f64) {
    if metered()
        && let Some(ring) = RING.get().and_then(|r| r.as_ref())
    {
        let o: Vec<f64> = OWN.iter().map(|o| o.swap(0, Relaxed) as f64 / frames).collect();
        let ms = |ticks: f64| ticks * 1000.0 / ring.frequency.max(1) as f64;
        tracing::info!(
            "the card's time in our own lists per frame (VICTORY_OWN_SPANS): {:.2} ms in {:.1} lists of sends to copies in its memory and their hash passes, \
             {:.2} ms in {:.1} compare lists",
            ms(o[1]),
            o[0],
            ms(o[3]),
            o[2]
        );
    }
    let [taken, read, missing] = [0, 1, 2].map(|i| STATS[i].swap(0, Relaxed) as f64 / frames);
    if taken + read > 0.0 {
        tracing::info!(
            "the title's GPU timestamps per frame (VICTORY_GPU_STAMPS): {taken:.1} taken on the card, {read:.1} read when their list had run, \
             {missing:.2} of them not written by the card (given the PC's clock)"
        );
    }
}

impl Host {
    fn make_true_stamps(&self) -> windows::core::Result<Ring> {
        let Some(buffer) = self.read_back_buffer(8 * u64::from(MOST)) else {
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
        };
        // SAFETY: plain object creation; a fresh mapping of that size.
        unsafe {
            let mut back = std::ptr::null_mut();
            buffer.Map(0, None, Some(&mut back))?;
            std::ptr::write_bytes(back.cast::<u8>(), 0, 8 * MOST as usize);
            let mut heap: Option<ID3D12QueryHeap> = None;
            self.device.CreateQueryHeap(
                &D3D12_QUERY_HEAP_DESC {
                    Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                    Count: MOST,
                    NodeMask: 0,
                },
                &mut heap,
            )?;
            let heap = heap.ok_or_else(windows::core::Error::empty)?;
            let frequency = self.queue.GetTimestampFrequency()?;
            let (mut card, mut pc) = (0, 0);
            self.queue.GetClockCalibration(&mut card, &mut pc)?;
            // (The PC's counter of that moment is this moment's, near enough:
            // the title measures spans.)
            let title = crate::queue::gpu_clock_now();
            Ok(Ring {
                heap,
                buffer,
                back: back as usize,
                frequency,
                calibrated: (card, title),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::placed;

    /// A card of 10 MHz ticks against the title's clock of 100 MHz: spans
    /// keep their length, before the calibration too.
    #[test]
    fn the_cards_ticks_are_put_on_the_titles_clock() {
        let calibrated = (1_000_000, 5_000_000_000);
        assert_eq!(placed(1_000_000, calibrated, 10_000_000), 5_000_000_000);
        // 33 ms later.
        assert_eq!(placed(1_330_000, calibrated, 10_000_000), 5_003_300_000);
        // 1 ms before.
        assert_eq!(placed(990_000, calibrated, 10_000_000), 4_999_900_000);
        // Never zero (zero says "not written").
        assert_eq!(placed(0, (1_000_000, 0), 10_000_000), 1);
    }
}
