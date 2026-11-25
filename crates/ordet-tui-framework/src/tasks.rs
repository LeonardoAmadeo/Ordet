//! Bounded persistent background [`TaskPool`] with cooperative cancellation
//! and panic isolation.

use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskKey(pub String);

impl From<&str> for TaskKey {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for TaskKey {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskHandle(pub u64);

#[derive(Clone, Debug)]
pub struct TaskError {
    pub message: String,
}

impl TaskError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TaskError {}

#[derive(Clone)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct TaskResult<Message> {
    pub handle: TaskHandle,
    pub key: Option<TaskKey>,
    pub result: Result<Message, TaskError>,
}

/// Default worker threads per pool: enough for the realistic mix of saves,
/// opens, and backups while a stalled job can't starve the rest.
const DEFAULT_WORKERS: usize = 6;

type Job<Message> = Box<dyn FnOnce(CancelToken) -> Result<Message, TaskError> + Send>;

pub struct TaskPool<Message> {
    receiver: Receiver<TaskResult<Message>>,
    jobs: Sender<(TaskHandle, Option<TaskKey>, CancelToken, Job<Message>)>,
    next_id: AtomicU64,
    active: HashMap<u64, Arc<AtomicBool>>,
    keyed: HashMap<TaskKey, TaskHandle>,
}

impl<Message> Default for TaskPool<Message>
where
    Message: Send + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Message> TaskPool<Message>
where
    Message: Send + 'static,
{
    /// Creates a pool with the default worker count (6).
    pub fn new() -> Self {
        Self::with_workers(DEFAULT_WORKERS)
    }

    /// Creates a persistent pool with `workers` threads (clamped to at least 1).
    /// Tasks run on threads that outlive them, so warm state can stay cached in
    /// thread-locals instead of being rebuilt on every request.
    pub fn with_workers(workers: usize) -> Self {
        let workers = workers.max(1);
        let (results, receiver) = unbounded();
        let (jobs, job_receiver) =
            unbounded::<(TaskHandle, Option<TaskKey>, CancelToken, Job<Message>)>();
        for index in 0..workers {
            let jobs = job_receiver.clone();
            let results = results.clone();
            let _ = thread::Builder::new()
                .name(format!("ordet-task-{index}"))
                .spawn(move || {
                    while let Ok((handle, key, token, job)) = jobs.recv() {
                        // A panicking task must not take its worker down
                        // (nor strand its handle as forever-active): it
                        // reports like any other failure.
                        let result = catch_unwind(AssertUnwindSafe(move || job(token)))
                            .unwrap_or_else(|_| Err(TaskError::new("task panicked")));
                        let _ = results.send(TaskResult {
                            handle,
                            key,
                            result,
                        });
                    }
                });
        }
        Self {
            receiver,
            jobs,
            next_id: AtomicU64::new(1),
            active: HashMap::new(),
            keyed: HashMap::new(),
        }
    }

    pub fn spawn<F>(&mut self, key: Option<TaskKey>, task: F) -> TaskHandle
    where
        F: FnOnce(CancelToken) -> Result<Message, TaskError> + Send + 'static,
    {
        if let Some(key) = &key {
            if let Some(handle) = self.keyed.get(key).copied() {
                self.cancel(handle);
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let handle = TaskHandle(id);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active.insert(id, cancelled.clone());
        if let Some(key) = &key {
            self.keyed.insert(key.clone(), handle);
        }

        let token = CancelToken { cancelled };
        let _ = self.jobs.send((handle, key, token, Box::new(task)));
        handle
    }

    pub fn try_recv(&mut self) -> Option<TaskResult<Message>> {
        let result = self.receiver.try_recv().ok()?;
        self.active.remove(&result.handle.0);
        if let Some(key) = &result.key {
            if self.keyed.get(key).copied() == Some(result.handle) {
                self.keyed.remove(key);
            }
        }
        Some(result)
    }

    pub fn cancel(&mut self, handle: TaskHandle) {
        if let Some(flag) = self.active.get(&handle.0) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Cancels the currently active task registered under `key`, if any.
    pub fn cancel_key(&mut self, key: &TaskKey) {
        if let Some(handle) = self.keyed.get(key).copied() {
            self.cancel(handle);
        }
    }

    /// Signals cancellation to every in-flight task.
    pub fn cancel_all(&mut self) {
        for flag in self.active.values() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Number of tasks currently in flight or awaiting `try_recv`.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Whether any spawned task is still in flight or has an unread
    /// result. The event loop polls on a short cadence while true, so
    /// landings are handled in milliseconds instead of at the next tick.
    pub fn has_active(&self) -> bool {
        !self.active.is_empty()
    }
}

impl<Message> Drop for TaskPool<Message> {
    fn drop(&mut self) {
        for flag in self.active.values() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{TaskError, TaskKey, TaskPool};

    #[test]
    fn a_panicking_task_reports_and_the_pool_survives() {
        let mut pool = TaskPool::<usize>::new();

        pool.spawn(None, |_| panic!("boom"));
        pool.spawn(None, |_| Ok(7usize));

        let deadline = Instant::now() + Duration::from_secs(1);
        let (mut saw_error, mut saw_seven) = (false, false);
        while Instant::now() < deadline && !(saw_error && saw_seven) {
            if let Some(result) = pool.try_recv() {
                match result.result {
                    Ok(7) => saw_seven = true,
                    Err(error) if error.message == "task panicked" => saw_error = true,
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }

        assert!(saw_error, "the panic should surface as a task error");
        assert!(saw_seven, "the pool should keep serving after a panic");
        assert!(!pool.has_active());
    }

    #[test]
    fn has_active_tracks_in_flight_tasks() {
        let mut pool = TaskPool::<usize>::new();
        assert!(!pool.has_active());
        assert_eq!(pool.active_count(), 0);

        pool.spawn(None, |_| {
            thread::sleep(Duration::from_millis(30));
            Ok(1usize)
        });
        assert!(pool.has_active());
        assert_eq!(pool.active_count(), 1);

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if pool.try_recv().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "result never arrived");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!pool.has_active());
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn replacing_a_keyed_task_cancels_the_previous_one() {
        let mut pool = TaskPool::new();

        pool.spawn(Some(TaskKey::from("save")), |token| {
            while !token.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            Err(TaskError::new("cancelled"))
        });

        pool.spawn(Some(TaskKey::from("save")), |_| Ok::<_, TaskError>(2usize));

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut saw_cancelled = false;
        let mut saw_success = false;
        while Instant::now() < deadline && !(saw_cancelled && saw_success) {
            if let Some(result) = pool.try_recv() {
                match result.result {
                    Ok(2) => saw_success = true,
                    Err(error) if error.message == "cancelled" => saw_cancelled = true,
                    _ => {}
                }
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }

        assert!(
            saw_cancelled,
            "expected the replaced keyed task to be cancelled"
        );
        assert!(saw_success, "expected the newest keyed task to finish");
    }
}
