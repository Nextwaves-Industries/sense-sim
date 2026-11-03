//! Worker threads for the time loop.
//!
//! A step is short, a few hundred microseconds on a small model, so a general task pool's sleep
//! and wake costs more than the work. These workers spin briefly between jobs instead, then yield,
//! so they neither waste a core for long nor stall when the machine is shared. Work is handed out
//! by slab from one counter: a worker that loses its core to something else just takes fewer.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

type Job = *const (dyn Fn(usize) + Sync);

struct Inner {
    /// Bumped to start a job.
    generation: AtomicUsize,
    /// The next slab to hand out.
    next: AtomicUsize,
    slabs: AtomicUsize,
    /// Workers that have finished the current job.
    finished: AtomicUsize,
    quit: AtomicBool,
    panicked: AtomicBool,
    job: UnsafeCell<Option<Job>>,
    workers: usize,
}

// SAFETY: `job` is written only while every worker is idle, and read only after the generation
// bump that publishes it; the pointer's target outlives the job because `run` waits for it.
unsafe impl Sync for Inner {}
unsafe impl Send for Inner {}

impl Inner {
    /// A panic has to stay inside: unwinding past here would drop what other workers are still
    /// reading, and a worker that never reports in would leave `run` waiting for ever.
    fn take_slabs(&self, job: &(dyn Fn(usize) + Sync)) {
        let total = self.slabs.load(Ordering::Acquire);
        loop {
            let s = self.next.fetch_add(1, Ordering::AcqRel);
            if s >= total {
                return;
            }
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(s))).is_err() {
                self.panicked.store(true, Ordering::Release);
            }
        }
    }
}

/// How long to spin before yielding the core. About the length of a short step.
const SPINS: u32 = 2_000;

pub struct Team {
    inner: Arc<Inner>,
    handles: Vec<JoinHandle<()>>,
}

impl Team {
    /// `threads` counts the calling thread, which works too.
    pub fn new(threads: usize) -> Self {
        let workers = threads.max(1) - 1;
        let inner = Arc::new(Inner {
            generation: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            slabs: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            quit: AtomicBool::new(false),
            panicked: AtomicBool::new(false),
            job: UnsafeCell::new(None),
            workers,
        });
        let handles = (0..workers)
            .map(|_| {
                let inner = inner.clone();
                std::thread::spawn(move || {
                    let mut seen = 0;
                    loop {
                        let mut spins = 0u32;
                        loop {
                            if inner.quit.load(Ordering::Acquire) {
                                return;
                            }
                            let g = inner.generation.load(Ordering::Acquire);
                            if g != seen {
                                seen = g;
                                break;
                            }
                            spins += 1;
                            if spins < SPINS {
                                std::hint::spin_loop();
                            } else {
                                std::thread::yield_now();
                            }
                        }
                        // SAFETY: published by the generation bump, and `run` keeps it alive
                        // until `finished` reaches every worker.
                        let job = unsafe { (*inner.job.get()).unwrap() };
                        inner.take_slabs(unsafe { &*job });
                        inner.finished.fetch_add(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        Self { inner, handles }
    }

    pub fn threads(&self) -> usize {
        self.inner.workers + 1
    }

    /// Calls `job(0..slabs)` across the team and returns when all of them are done.
    pub fn run(&self, slabs: usize, job: &(dyn Fn(usize) + Sync)) {
        let inner = &self.inner;
        if inner.workers == 0 || slabs <= 1 {
            (0..slabs).for_each(job);
            return;
        }
        // SAFETY: the lifetime is erased only for the duration of this call, which waits for every
        // worker below; no job is live now, as the last call also waited.
        unsafe {
            let erased: Job = std::mem::transmute::<*const (dyn Fn(usize) + Sync + '_), Job>(job as *const _);
            *inner.job.get() = Some(erased);
        }
        inner.slabs.store(slabs, Ordering::Release);
        inner.next.store(0, Ordering::Release);
        inner.finished.store(0, Ordering::Release);
        inner.generation.fetch_add(1, Ordering::AcqRel);
        inner.take_slabs(job);
        let mut spins = 0u32;
        while inner.finished.load(Ordering::Acquire) < inner.workers {
            spins += 1;
            if spins < SPINS {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        // Every worker has let go of the job by now, so this is safe to raise here.
        if inner.panicked.swap(false, Ordering::AcqRel) {
            panic!("a worker panicked during a time step");
        }
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        self.inner.quit.store(true, Ordering::Release);
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn every_slab_runs_exactly_once_many_times_over() {
        let team = Team::new(8);
        let total = AtomicU64::new(0);
        for _ in 0..20_000 {
            team.run(32, &|s| {
                total.fetch_add(s as u64 + 1, Ordering::Relaxed);
            });
        }
        assert_eq!(total.load(Ordering::Relaxed), 20_000 * (32 * 33 / 2));
    }

    #[test]
    #[should_panic(expected = "a worker panicked")]
    fn a_panic_in_a_slab_comes_out_of_run_and_does_not_hang() {
        let team = Team::new(4);
        team.run(16, &|s| {
            if s == 9 {
                panic!("slab 9");
            }
        });
    }

    #[test]
    fn a_team_of_one_still_works() {
        let team = Team::new(1);
        let total = AtomicU64::new(0);
        team.run(5, &|_| {
            total.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(total.load(Ordering::Relaxed), 5);
    }
}
