//! Optional keystroke latency log.
//!
//! Off unless `ORDET_TUI_LATENCY=1` (set by `--latency-log`). Measured span
//! is one keystroke: from `event::read` returning to `stdout.flush()`
//! returning. That is the CPU + write-syscall leg the event-driven target is
//! defined against; it deliberately excludes the terminal emulator's own
//! presentation delay.
//!
//! Each sample also carries its own decomposition, `read` (the `event::read`
//! call itself), `handle` (key dispatch) and `draw` (layout, render, diff,
//! write, flush), so a report can show where the time went without the
//! measuring instrument itself writing anything per keystroke.

use std::sync::Mutex;
use std::time::Duration;

/// How many samples between reports, so a long session prints a stable p99
/// without stderr traffic per keystroke.
const REPORT_EVERY: usize = 200;
/// First keystrokes after launch pay one-time costs (font probes, rope
/// warmup, first wrap of the visible rows). They are dropped so the numbers
/// describe the steady state.
const WARMUP_KEYS: usize = 10;

#[derive(Clone, Copy)]
struct Sample {
    total: Duration,
    read: Duration,
    handle: Option<Duration>,
    draw: Option<Duration>,
    /// Key events handled in this batch: >= 1. A batch is one wakeup plus
    /// every event that was already queued behind the first one.
    batch: u32,
}

static SAMPLES: Mutex<Vec<Sample>> = Mutex::new(Vec::new());
static IGNORED: Mutex<usize> = Mutex::new(0);

/// Whether the latency log was requested for this process.
pub fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // `--latency-log` (or the env var) is read at first use, after argument
    // parsing has normalized it into the environment.
    *ENABLED.get_or_init(|| std::env::var_os("ORDET_TUI_LATENCY").is_some())
}

/// Enable the log before the argument parser runs.
pub fn enable_from_flag(flag: bool) {
    if flag {
        // Startup is single-threaded here; no task has spawned yet.
        unsafe {
            std::env::set_var("ORDET_TUI_LATENCY", "1");
        }
    }
}

/// Record one span together with its read, input-handling and draw halves.
pub fn record_parts(total: Duration, read: Duration, handle: Duration, draw: Duration, batch: u32) {
    push(Sample {
        total,
        read,
        handle: Some(handle),
        draw: Some(draw),
        batch: batch.max(1),
    });
}

fn push(sample: Sample) {
    if !enabled() {
        return;
    }
    if let Ok(mut ignored) = IGNORED.lock() {
        if *ignored < WARMUP_KEYS {
            *ignored += 1;
            return;
        }
    }
    if let Ok(mut samples) = SAMPLES.lock() {
        samples.push(sample);
        if samples.len() >= REPORT_EVERY {
            report(&samples);
            samples.clear();
        }
    }
}

/// Print the final summary. Called once on the way out of `run`.
pub fn report_final() {
    if !enabled() {
        return;
    }
    if let Ok(mut samples) = SAMPLES.lock() {
        if !samples.is_empty() {
            report(&samples);
            samples.clear();
        }
    }
}

fn report(samples: &[Sample]) {
    if samples.is_empty() {
        return;
    }
    let Some((p50, p90, p99, max, mean)) = percentiles(samples.iter().map(|sample| sample.total))
    else {
        return;
    };
    let batch_mean = samples
        .iter()
        .map(|sample| f64::from(sample.batch))
        .sum::<f64>()
        / samples.len() as f64;
    eprint!(
        "[latency] keys={} p50={p50}us p90={p90}us p99={p99}us max={max}us mean={mean:.1}us",
        samples.len(),
    );
    if let Some((h50, _, h99, _, _)) = percentiles(samples.iter().filter_map(|s| s.handle)) {
        let (d50, _, d99, _, _) =
            percentiles(samples.iter().filter_map(|s| s.draw)).unwrap_or_default();
        let (r50, _, _, _, _) = percentiles(samples.iter().map(|s| s.read)).unwrap_or_default();
        eprint!(
            " | work read p50={r50}us handle p50={h50}us p99={h99}us draw p50={d50}us p99={d99}us batch={batch_mean:.2}"
        );
    }
    eprintln!();
}

/// Percentiles in microseconds: (p50, p90, p99, max, mean).
fn percentiles(values: impl Iterator<Item = Duration>) -> Option<(u128, u128, u128, u128, f64)> {
    let mut micros: Vec<u128> = values.map(|value| value.as_micros()).collect();
    if micros.is_empty() {
        return None;
    }
    micros.sort_unstable();
    let at = |p: usize| micros[(micros.len() * p / 100).min(micros.len() - 1)];
    let mean = micros.iter().sum::<u128>() as f64 / micros.len() as f64;
    Some((at(50), at(90), at(99), micros[micros.len() - 1], mean))
}
