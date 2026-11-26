//! What `victory shaders` and `victory warm` say of how far they are.
//!
//! With `--progress` both print `PROGRESS <done> <total>` lines. Without it
//! (an older `victory.exe`):
//!
//! - `warm` begins with `N pipelines known, M made before (..), B bad; K to
//!   make, J processes`, then says `C of K, S s` every thousand, and ends
//!   with `C made in S s (..)`: done and total, coarsely;
//! - `shaders` says `C shaders in the cache, S s` every thousand and ends
//!   with `C shaders in the cache, F do not translate, S s`: a count without
//!   a total.

use std::collections::VecDeque;
use std::time::Duration;

/// What one line of output tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// So many of so many.
    Of { done: u64, total: u64 },
    /// So many, of an unknown number.
    Count { done: u64 },
    /// So many are to do in all.
    Total { total: u64 },
    /// The step's own last line: it came to its end.
    Finished { done: u64 },
}

fn leading_number(text: &str) -> Option<(u64, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let n = text[..end].parse().ok()?;
    Some((n, &text[end..]))
}

pub fn parse_line(line: &str) -> Option<Told> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("PROGRESS ") {
        let mut words = rest.split_ascii_whitespace();
        let done = words.next()?.parse().ok()?;
        let total = words.next()?.parse().ok()?;
        return Some(Told::Of { done, total });
    }
    let (n, rest) = leading_number(line)?;
    if let Some(rest) = rest.strip_prefix(" of ") {
        let (total, _) = leading_number(rest)?;
        return Some(Told::Of { done: n, total });
    }
    if rest.starts_with(" pipelines known,") {
        let (_, after) = rest.split_once("; ")?;
        let (total, tail) = leading_number(after)?;
        return tail
            .starts_with(" to make")
            .then_some(Told::Total { total });
    }
    if rest.starts_with(" made in ") {
        return Some(Told::Finished { done: n });
    }
    if rest.starts_with(" shaders in the cache,") {
        return Some(if rest.contains("do not translate") {
            Told::Finished { done: n }
        } else {
            Told::Count { done: n }
        });
    }
    None
}

/// How far a step is, and how fast it goes.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    pub finished: bool,
    /// (When, how many were done then), the last minute of them.
    samples: VecDeque<(Duration, u64)>,
}

/// The rate is taken over this much of the recent past.
const WINDOW: Duration = Duration::from_secs(60);

impl Progress {
    /// Takes in a line of output said `at` this long after the start.
    pub fn feed(&mut self, line: &str, at: Duration) -> bool {
        let Some(told) = parse_line(line) else {
            return false;
        };
        match told {
            Told::Of { done, total } => {
                self.done = done;
                self.total = Some(total);
            }
            Told::Count { done } => self.done = done,
            Told::Total { total } => self.total = Some(total),
            Told::Finished { done } => {
                self.done = done;
                self.finished = true;
            }
        }
        if !matches!(told, Told::Total { .. }) {
            self.samples.push_back((at, self.done));
            while self.samples.len() > 2
                && self
                    .samples
                    .front()
                    .is_some_and(|&(t, _)| at.saturating_sub(t) > WINDOW)
            {
                self.samples.pop_front();
            }
        }
        true
    }

    /// 0 to 1, when a total is known.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(0) => Some(1.0),
            Some(total) => Some((self.done as f32 / total as f32).clamp(0.0, 1.0)),
            None => None,
        }
    }

    /// Items a second over the recent past; `None` until two reports differ
    /// in time.
    pub fn rate(&self) -> Option<f64> {
        let (&(t0, d0), &(t1, d1)) = (self.samples.front()?, self.samples.back()?);
        let secs = t1.saturating_sub(t0).as_secs_f64();
        (secs > 0.0 && d1 > d0).then(|| (d1 - d0) as f64 / secs)
    }

    /// How long the rest takes at that rate.
    pub fn eta(&self) -> Option<Duration> {
        let left = self.total?.saturating_sub(self.done);
        if left == 0 {
            return Some(Duration::ZERO);
        }
        Some(Duration::from_secs_f64(left as f64 / self.rate()?))
    }
}

/// `1:05`, `2:03:09`.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `12 345`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// `1.5 GB`, `340 MB`.
pub fn bytes(n: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let n = n as f64;
    if n >= GB {
        format!("{:.1} GB", n / GB)
    } else if n >= 1024.0 * 1024.0 {
        format!("{:.0} MB", n / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", n / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_lines() {
        assert_eq!(
            parse_line("PROGRESS 120 4800"),
            Some(Told::Of {
                done: 120,
                total: 4800
            })
        );
        assert_eq!(
            parse_line("  PROGRESS 0 0\r"),
            Some(Told::Of { done: 0, total: 0 })
        );
        assert_eq!(parse_line("PROGRESS x 3"), None);
        assert_eq!(parse_line("PROGRESS 3"), None);
    }

    #[test]
    fn warm_lines_as_victory_prints_them() {
        assert_eq!(
            parse_line(
                "48211 pipelines known, 1200 made before (7f3a9c1e), 3 bad; 5320 to make, 16 processes"
            ),
            Some(Told::Total { total: 5320 })
        );
        assert_eq!(
            parse_line("3000 of 5320, 412 s"),
            Some(Told::Of {
                done: 3000,
                total: 5320
            })
        );
        assert_eq!(
            parse_line(
                "5317 made in 733 s (12 have no host pipeline, 3 ended their process); ms each: median 41.0, p90 310.2, p99 2011.9, most 9120, sum 11021 s"
            ),
            Some(Told::Finished { done: 5317 })
        );
        assert_eq!(parse_line("pipeline 17 0123abcd: hung for 180 s"), None);
    }

    #[test]
    fn shader_lines_as_victory_prints_them() {
        assert_eq!(
            parse_line("4000 shaders in the cache, 37 s"),
            Some(Told::Count { done: 4000 })
        );
        assert_eq!(
            parse_line("91532 shaders in the cache, 14 do not translate, 512 s"),
            Some(Told::Finished { done: 91532 })
        );
        assert_eq!(
            parse_line("     14  unsupported opcode  (e.g. 0123456789abcdef)"),
            None
        );
    }

    #[test]
    fn other_output_is_not_progress() {
        for line in [
            "",
            "2026-11-25T10:00:00Z  INFO victory: starting",
            "12 angry lines",
            "of 3",
            "made 4 12.5 1",
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    #[test]
    fn fraction_rate_and_eta() {
        let mut p = Progress::default();
        assert_eq!(p.fraction(), None);
        assert!(p.feed(
            "100 pipelines known, 0 made before (x), 0 bad; 1000 to make, 4 processes",
            Duration::ZERO
        ));
        assert_eq!(p.fraction(), Some(0.0));
        assert_eq!(p.rate(), None);
        assert!(p.feed("PROGRESS 100 1000", Duration::from_secs(10)));
        assert!(p.feed("PROGRESS 300 1000", Duration::from_secs(20)));
        assert_eq!(p.fraction(), Some(0.3));
        assert_eq!(p.rate(), Some(20.0));
        assert_eq!(p.eta(), Some(Duration::from_secs(35)));
        assert!(!p.feed("something else", Duration::from_secs(21)));
        assert!(p.feed(
            "1000 made in 60 s (0 have no host pipeline, 0 ended their process)",
            Duration::from_secs(60)
        ));
        assert!(p.finished);
        assert_eq!(p.eta(), Some(Duration::ZERO));
    }

    #[test]
    fn a_count_without_a_total_has_a_rate_but_no_fraction() {
        let mut p = Progress::default();
        p.feed("1000 shaders in the cache, 5 s", Duration::from_secs(5));
        p.feed("3000 shaders in the cache, 15 s", Duration::from_secs(15));
        assert_eq!(p.fraction(), None);
        assert_eq!(p.rate(), Some(200.0));
        assert_eq!(p.eta(), None);
    }

    #[test]
    fn the_rate_is_of_the_recent_past() {
        let mut p = Progress::default();
        p.feed("PROGRESS 0 100000", Duration::ZERO);
        p.feed("PROGRESS 10 100000", Duration::from_secs(100));
        p.feed("PROGRESS 1010 100000", Duration::from_secs(110));
        p.feed("PROGRESS 2010 100000", Duration::from_secs(120));
        // The slow first hundred seconds have dropped out.
        assert_eq!(p.rate(), Some(100.0));
    }

    #[test]
    fn words_for_numbers() {
        assert_eq!(clock(Duration::from_secs(65)), "1:05");
        assert_eq!(clock(Duration::from_secs(7389)), "2:03:09");
        assert_eq!(grouped(1234567), "1 234 567");
        assert_eq!(grouped(999), "999");
        assert_eq!(bytes(1536 * 1024 * 1024), "1.5 GB");
        assert_eq!(bytes(340 * 1024 * 1024), "340 MB");
    }
}
