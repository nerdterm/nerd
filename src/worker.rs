//! A worker: a thread that takes jobs off the loop and hands back results, ringing the loop's bell after each.

use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Worker<Q, R> {
    jobs: Sender<Q>,
    results: Receiver<R>,
}

impl<Q: Send + 'static, R: Send + 'static> Worker<Q, R> {
    /// `work` gets every job queued since it last ran, oldest first, so it can drop the ones a newer job made moot;
    /// when it panics, `failed` answers each job of that batch instead, so nobody waits on an answer that never comes.
    pub fn spawn(
        name: &'static str,
        bell: crate::bell::Bell,
        mut work: impl FnMut(Vec<Q>) -> Vec<R> + Send + 'static,
        failed: impl Fn(&Q) -> Option<R> + Send + 'static,
    ) -> Self {
        let (jobs, inbox) = channel::<Q>();
        let (out, results) = channel();
        let run = move || {
            while let Ok(job) = inbox.recv() {
                let batch: Vec<Q> = std::iter::once(job).chain(inbox.try_iter()).collect();
                let fallback: Vec<R> = batch.iter().filter_map(&failed).collect();
                let done = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(batch))) {
                    Ok(done) => done,
                    Err(e) => {
                        let why = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
                        crate::log::error(&format!("worker {name} failed: {why}"));
                        fallback
                    }
                };
                for r in done {
                    if out.send(r).is_err() {
                        return;
                    }
                }
                bell.ring();
            }
        };
        if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(run) {
            crate::log::error(&format!("worker {name} did not start: {e}"));
        }
        Self { jobs, results }
    }

    pub fn ask(&self, job: Q) {
        let _ = self.jobs.send(job);
    }

    /// Results so far, without waiting: the loop calls this on every wake.
    pub fn take(&self) -> impl Iterator<Item = R> + '_ {
        self.results.try_iter()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn a_failed_job_is_one_line_in_the_log_is_still_answered_and_the_worker_goes_on() {
        crate::session::tests::with_temp_state_dir("worker-fails", || {
            let (bell, door) = crate::bell::new();
            let w = super::Worker::spawn(
                "test",
                bell,
                |jobs: Vec<u32>| {
                    jobs.into_iter()
                        .map(|j| {
                            assert!(j != 0, "no zero");
                            j * 2
                        })
                        .collect()
                },
                |j| Some(1000 + j),
            );
            let next = || {
                let t = Instant::now();
                loop {
                    door.wait(Duration::from_millis(100));
                    if let Some(r) = w.take().next() {
                        break r;
                    }
                    assert!(t.elapsed() < Duration::from_secs(5), "no answer");
                }
            };
            w.ask(0);
            assert_eq!(next(), 1000, "the failed job is answered as failed");
            w.ask(21);
            assert_eq!(next(), 42);
            let log = std::fs::read_to_string(crate::session::state_dir().join("daemon.log")).unwrap();
            let lines: Vec<&str> = log.lines().filter(|l| l.contains("worker test failed")).collect();
            assert_eq!(lines.len(), 1, "{log}");
            assert!(lines[0].ends_with("worker test failed: no zero"), "{log}");
        });
    }
}
