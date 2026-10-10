//! Holding heavy background work to a few at a time, first come first served.
//!
//! Importing 200 files used to start 200 probes and conversions at once (and the bin as
//! many filmstrip readers): the CPU and the disk thrashed, and the editor stuttered. A
//! [`Gate`] lets `max` jobs run at once, in the order they were queued — a job takes a
//! ticket when it's queued (on the UI thread, so in order) and waits on its worker thread
//! for its turn. A ticket that's dropped unused (the job found its answer in a cache)
//! gives its place up, so the line never waits on it.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

pub struct Gate {
    max: usize,
    tickets: AtomicU64,
    state: Mutex<Line>,
    turn: Condvar,
}

/// Where the line is.
struct Line {
    /// The next ticket allowed to start.
    next: u64,
    running: usize,
    /// Tickets given up before their turn: skipped when it comes.
    given_up: BTreeSet<u64>,
}

impl Line {
    /// On to the next ticket still wanting its turn.
    fn advance(&mut self) {
        self.next += 1;
        while self.given_up.remove(&self.next) {
            self.next += 1;
        }
    }
}

/// A job's place in line; [`Ticket::wait`] blocks until it's its turn. Dropped unused,
/// it gives its place up.
pub struct Ticket<'a> {
    gate: &'a Gate,
    number: u64,
    used: bool,
}

/// Held while a job runs: dropping it lets the next one in.
pub struct Pass<'a> {
    gate: &'a Gate,
}

impl Gate {
    pub fn new(max: usize) -> Self {
        Gate { max: max.max(1), tickets: AtomicU64::new(0), state: Mutex::new(Line { next: 0, running: 0, given_up: BTreeSet::new() }), turn: Condvar::new() }
    }

    /// Joins the line (call it when the job is queued, so the order is the queuing order).
    pub fn ticket(&self) -> Ticket<'_> {
        Ticket { gate: self, number: self.tickets.fetch_add(1, Ordering::SeqCst), used: false }
    }
}

impl<'a> Ticket<'a> {
    /// Waits until every job queued before this one has started (or given up) and fewer
    /// than `max` are running, then runs (until the pass is dropped).
    pub fn wait(mut self) -> Pass<'a> {
        self.used = true;
        let g = self.gate;
        let mut s = g.state.lock().unwrap_or_else(|e| e.into_inner());
        while s.next != self.number || s.running >= g.max {
            s = g.turn.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.advance();
        s.running += 1;
        g.turn.notify_all();
        Pass { gate: g }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if self.used {
            return;
        }
        let mut s = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.next == self.number {
            s.advance();
        } else {
            s.given_up.insert(self.number);
        }
        self.gate.turn.notify_all();
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        let mut s = self.gate.state.lock().unwrap_or_else(|e| e.into_inner());
        s.running -= 1;
        self.gate.turn.notify_all();
    }
}

/// How many imports (probing, converting, fingerprinting) run at once: half the cores,
/// two to four.
pub fn imports() -> &'static Gate {
    static GATE: std::sync::OnceLock<Gate> = std::sync::OnceLock::new();
    GATE.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Gate::new((cores / 2).clamp(2, 4))
    })
}

/// How many filmstrips and waveforms are read at once.
pub fn previews() -> &'static Gate {
    static GATE: std::sync::OnceLock<Gate> = std::sync::OnceLock::new();
    GATE.get_or_init(|| Gate::new(2))
}

#[cfg(test)]
mod tests {
    use super::Gate;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// No more than `max` run at once, and they start in the order they were queued.
    #[test]
    fn jobs_run_a_few_at_a_time_in_order() {
        let gate: &'static Gate = Box::leak(Box::new(Gate::new(2)));
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut threads = Vec::new();
        // Tickets taken in order 0..8; the threads are started back to front.
        let tickets: Vec<_> = (0..8).map(|i| (i, gate.ticket())).collect();
        for (i, ticket) in tickets.into_iter().rev() {
            let (running, most, started) = (running.clone(), most.clone(), started.clone());
            threads.push(std::thread::spawn(move || {
                let _pass = ticket.wait();
                started.lock().unwrap().push(i);
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(15));
                running.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert!(most.load(Ordering::SeqCst) <= 2, "at most two at once");
        // Let in in order; two let in together may note it in either order.
        let started = started.lock().unwrap();
        assert!(started.iter().enumerate().all(|(k, &i)| k.abs_diff(i) <= 1), "in the order queued: {started:?}");
    }

    /// A ticket dropped unused — first in line, or further back — holds nobody up.
    #[test]
    fn a_ticket_given_up_holds_nobody_up() {
        let gate = Gate::new(1);
        let (a, b, c, d) = (gate.ticket(), gate.ticket(), gate.ticket(), gate.ticket());
        drop(a); // first in line, given up
        drop(c); // further back, given up
        let pass = b.wait();
        drop(pass);
        let _pass = d.wait(); // past the given-up `c`
    }
}
