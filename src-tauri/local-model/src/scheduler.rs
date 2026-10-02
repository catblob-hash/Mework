//! One thread owns the backend and runs every request.
//!
//! Requests wait in a bounded queue and are admitted into free slots; all
//! live slots then advance together, one token per step, so concurrent
//! requests share each pass over the weights; how many run at once is the
//! backend's slot count, which bounds the batch. Identical requests that are
//! waiting or running are merged into one. Requests come in two lanes: the
//! background (a subagent's) waits behind the foreground (the conversation
//! itself) and gives way when the queue is full. When nothing is live the
//! per-sequence buffers are dropped right away, and the weights after an idle
//! period or when the system reports memory pressure; the next request loads
//! them again.
//!
//! The thread publishes its status as it changes, so reading it never waits
//! behind a load (which on the Neural Engine can take minutes) or a request.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{greedy, Backend, Capacity, Segment, Specials};
use crate::prefix_cache::PrefixCache;

pub type Loader = Box<dyn FnMut() -> Result<Box<dyn Backend>, String> + Send>;
pub type Reply<T> = Box<dyn FnOnce(Result<T, String>) + Send>;
/// Told, on the scheduler thread, when the model starts or stops loading,
/// loads, unloads or fails.
pub type Observer = Arc<dyn Fn(&Status) + Send + Sync>;
/// Called with the tokens generated so far; true ends the sequence.
pub type StopWhen = Arc<dyn Fn(&[u32]) -> bool + Send + Sync>;

pub const BUSY: &str = "本地模型繁忙，请稍后再试";

/// Whom a request serves. The conversation in front of the user goes first;
/// what its subagents ask for waits behind it, and is what a full queue
/// refuses to make room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lane {
    #[default]
    Foreground,
    Background,
}

pub struct Generate {
    /// The system prompt's tokens; its state comes from the prefix cache.
    pub prefix: Arc<Vec<u32>>,
    pub input: Arc<Vec<Segment>>,
    /// Sequence positions `input` takes (`Segment::len`).
    pub positions: usize,
    /// Identifies the request: equal keys (and prefixes) are merged.
    pub key: String,
    pub max_new: usize,
    pub stop_when: StopWhen,
    pub lane: Lane,
    pub reply: Reply<Vec<u32>>,
}

pub struct Limits {
    /// Requests waiting beyond this are refused (a background one in a
    /// foreground request's place).
    pub queue: usize,
    /// Unload the weights after this long with nothing to do.
    pub unload_after: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self { queue: 32, unload_after: Duration::from_secs(300) }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub loaded: bool,
    /// The weights are being loaded (on the Neural Engine, compiled first
    /// when the system has no compiled copy for this app).
    pub loading: bool,
    pub device: Option<String>,
    pub slots: usize,
    pub context: usize,
    pub running: usize,
    pub queued: usize,
    pub last_error: Option<String>,
}

/// What caching a prefix produced.
#[derive(Clone, Copy, Debug)]
pub struct PrefixSummary {
    pub tokens: usize,
    pub bytes: u64,
}

enum Message {
    Generate(Generate),
    Prefix { tokens: Vec<u32>, reply: Reply<PrefixSummary> },
    /// Delete cached prefix states other than these prompts'.
    Prune { keep: Vec<Vec<u32>> },
    /// Drop the weights as soon as nothing is running (memory pressure).
    Unload,
    Shutdown,
}

/// What the scheduler thread publishes for everyone else to read.
#[derive(Default)]
struct Shared {
    status: Mutex<Status>,
    observer: Mutex<Option<Observer>>,
}

pub struct Scheduler {
    sender: Sender<Message>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Scheduler {
    pub fn start(loader: Loader, specials: Specials, limits: Limits, cache_dir: std::path::PathBuf) -> Self {
        let (sender, receiver) = channel();
        let shared = Arc::new(Shared::default());
        let worker_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("mework-local-model".into())
            .spawn(move || {
                Worker::new(loader, specials, limits, PrefixCache::new(cache_dir), receiver)
                    .with_shared(worker_shared)
                    .run()
            })
            .expect("spawn local model thread");
        Self { sender, shared, thread: Some(thread) }
    }

    pub fn generate(&self, job: Generate) {
        if let Err(error) = self.sender.send(Message::Generate(job)) {
            if let Message::Generate(job) = error.0 {
                (job.reply)(Err("本地模型已停止".into()));
            }
        }
    }

    /// Makes sure the state after `tokens` is cached; loads the model if needed.
    pub fn prefix(&self, tokens: Vec<u32>, reply: Reply<PrefixSummary>) {
        if let Err(error) = self.sender.send(Message::Prefix { tokens, reply }) {
            if let Message::Prefix { reply, .. } = error.0 {
                reply(Err("本地模型已停止".into()));
            }
        }
    }

    /// The status as the scheduler thread last published it; never waits.
    pub fn status(&self) -> Status {
        self.shared.status.lock().expect("local model status").clone()
    }

    /// Calls `observer` whenever the model starts or stops loading, loads,
    /// unloads or records an error.
    pub fn observe(&self, observer: Observer) {
        *self.shared.observer.lock().expect("local model observer") = Some(observer);
    }

    pub fn unload(&self) {
        let _ = self.sender.send(Message::Unload);
    }

    pub fn prune(&self, keep: Vec<Vec<u32>>) {
        let _ = self.sender.send(Message::Prune { keep });
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Pending {
    prefix: Arc<Vec<u32>>,
    input: Arc<Vec<Segment>>,
    positions: usize,
    key: String,
    max_new: usize,
    stop_when: StopWhen,
    lane: Lane,
    replies: Vec<Reply<Vec<u32>>>,
}

impl Pending {
    fn same_request(&self, job: &Generate) -> bool {
        self.key == job.key && self.prefix == job.prefix && self.max_new == job.max_new
    }

    fn finish(self, result: Result<Vec<u32>, String>) {
        for reply in self.replies {
            reply(result.clone());
        }
    }
}

struct Live {
    request: Pending,
    output: Vec<u32>,
}

struct Worker {
    loader: Loader,
    specials: Specials,
    limits: Limits,
    receiver: Receiver<Message>,
    backend: Option<Box<dyn Backend>>,
    cache: PrefixCache,
    capacity: Capacity,
    queue: VecDeque<Pending>,
    slots: Vec<Option<Live>>,
    idle_since: Instant,
    trimmed: bool,
    unload_requested: bool,
    last_error: Option<String>,
    loading: bool,
    shared: Arc<Shared>,
}

impl Worker {
    fn new(loader: Loader, specials: Specials, limits: Limits, cache: PrefixCache, receiver: Receiver<Message>) -> Self {
        Self {
            loader,
            specials,
            limits,
            receiver,
            backend: None,
            cache,
            capacity: Capacity { slots: 0, context: 0 },
            queue: VecDeque::new(),
            slots: Vec::new(),
            idle_since: Instant::now(),
            trimmed: true,
            unload_requested: false,
            last_error: None,
            loading: false,
            shared: Arc::default(),
        }
    }

    fn with_shared(mut self, shared: Arc<Shared>) -> Self {
        self.shared = shared;
        self
    }

    fn snapshot(&self) -> Status {
        Status {
            loaded: self.backend.is_some(),
            loading: self.loading,
            device: self.backend.as_ref().map(|backend| backend.device()),
            slots: self.capacity.slots,
            context: self.capacity.context,
            running: self.running(),
            queued: self.queue.len(),
            last_error: self.last_error.clone(),
        }
    }

    /// Publishes the status; tells the observer when more than the request
    /// counts changed.
    fn publish(&self) {
        let status = self.snapshot();
        let notable = {
            let mut shared = self.shared.status.lock().expect("local model status");
            let notable = shared.loaded != status.loaded
                || shared.loading != status.loading
                || shared.device != status.device
                || shared.last_error != status.last_error;
            *shared = status.clone();
            notable
        };
        if notable {
            let observer = self.shared.observer.lock().expect("local model observer").clone();
            if let Some(observer) = observer {
                observer(&status);
            }
        }
    }

    fn running(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    fn run(mut self) {
        loop {
            self.publish();
            let busy = self.running() > 0 || !self.queue.is_empty();
            let message = if busy {
                match self.receiver.try_recv() {
                    Ok(message) => Some(message),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return self.shutdown(),
                }
            } else {
                self.go_idle();
                let wait = if self.backend.is_some() {
                    self.limits.unload_after.saturating_sub(self.idle_since.elapsed())
                } else {
                    Duration::from_secs(3600)
                };
                match self.receiver.recv_timeout(wait) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => {
                        if self.backend.is_some() && self.idle_since.elapsed() >= self.limits.unload_after {
                            self.unload();
                        }
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => return self.shutdown(),
                }
            };
            if let Some(message) = message {
                if !self.handle(message) {
                    return self.shutdown();
                }
                continue; // drain everything that arrived before doing work
            }
            if busy {
                self.admit_one();
                self.step();
            }
        }
    }

    /// Returns false on shutdown.
    fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Generate(job) => self.enqueue(job),
            Message::Prefix { tokens, reply } => {
                let result = match self.ensure_backend() {
                    Ok(_) => {
                        let backend = self.backend.as_mut().expect("backend");
                        self.cache.get(backend.as_mut(), &tokens)
                    }
                    Err(error) => Err(error),
                };
                let result = result.map(|state| PrefixSummary { tokens: state.tokens, bytes: state.bytes.len() as u64 });
                if let Err(error) = &result {
                    self.last_error = Some(error.clone());
                }
                reply(result);
                self.trimmed = false;
            }
            Message::Prune { keep } => {
                if let Some(backend) = self.backend.as_ref() {
                    let format = backend.state_format();
                    self.cache.prune(&format, &keep);
                }
            }
            Message::Unload => {
                self.unload_requested = true;
                if self.running() == 0 && self.queue.is_empty() {
                    self.unload();
                    self.unload_requested = false;
                }
            }
            Message::Shutdown => return false,
        }
        true
    }

    fn enqueue(&mut self, job: Generate) {
        if let Some(live) = self.slots.iter_mut().flatten().find(|live| live.request.same_request(&job)) {
            live.request.replies.push(job.reply);
            return;
        }
        if let Some(pending) = self.queue.iter_mut().find(|pending| pending.same_request(&job)) {
            pending.replies.push(job.reply);
            // Someone in front is waiting on it now.
            if job.lane == Lane::Foreground {
                pending.lane = Lane::Foreground;
            }
            return;
        }
        if self.queue.len() >= self.limits.queue {
            // The newest background request gives its place to the foreground.
            let evicted = match job.lane {
                Lane::Foreground => self.queue.iter().rposition(|pending| pending.lane == Lane::Background),
                Lane::Background => None,
            };
            match evicted.and_then(|index| self.queue.remove(index)) {
                Some(pending) => pending.finish(Err(BUSY.into())),
                None => {
                    (job.reply)(Err(BUSY.into()));
                    return;
                }
            }
        }
        self.queue.push_back(Pending {
            prefix: job.prefix,
            input: job.input,
            positions: job.positions,
            key: job.key,
            max_new: job.max_new,
            stop_when: job.stop_when,
            lane: job.lane,
            replies: vec![job.reply],
        });
    }

    /// The waiting request to admit next: the oldest in the foreground, else
    /// the oldest.
    fn next_waiting(&self) -> usize {
        self.queue.iter().position(|pending| pending.lane == Lane::Foreground).unwrap_or(0)
    }

    fn ensure_backend(&mut self) -> Result<&mut Box<dyn Backend>, String> {
        if self.backend.is_none() {
            self.loading = true;
            self.publish();
            let loaded = (self.loader)();
            self.loading = false;
            let backend = loaded?;
            self.capacity = backend.capacity();
            self.slots = (0..self.capacity.slots).map(|_| None).collect();
            self.backend = Some(backend);
            self.publish();
        }
        Ok(self.backend.as_mut().expect("backend"))
    }

    fn fail_all(&mut self, error: String) {
        self.last_error = Some(error.clone());
        for pending in self.queue.drain(..) {
            pending.finish(Err(error.clone()));
        }
        for slot in &mut self.slots {
            if let Some(live) = slot.take() {
                live.request.finish(Err(error.clone()));
            }
        }
    }

    fn admit_one(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        if let Err(error) = self.ensure_backend() {
            return self.fail_all(error);
        }
        let Some(slot) = self.slots.iter().position(Option::is_none) else { return };
        let request = self.queue.remove(self.next_waiting()).expect("queued");
        let capacity = self.capacity;
        if request.prefix.len() + request.positions + request.max_new > capacity.context || request.positions == 0 {
            return request.finish(Err("请求超出本地模型上下文".into()));
        }
        self.trimmed = false;
        let backend = self.backend.as_mut().expect("backend");
        let prefix = match self.cache.get(backend.as_mut(), &request.prefix) {
            Ok(prefix) => prefix,
            Err(error) => {
                self.last_error = Some(error.clone());
                return request.finish(Err(error));
            }
        };
        match backend.admit(slot, &prefix, &request.input) {
            Ok(logits) => {
                let token = greedy(&logits, &self.specials);
                self.slots[slot] = Some(Live { request, output: Vec::new() });
                self.accept(slot, token);
            }
            Err(error) => {
                backend.release(slot);
                self.last_error = Some(error.clone());
                request.finish(Err(error));
            }
        }
    }

    /// Records `token` for `slot`, finishing the sequence if it is done.
    fn accept(&mut self, slot: usize, token: u32) {
        let live = self.slots[slot].as_mut().expect("live slot");
        let done = if self.specials.is_stop(token) {
            true
        } else {
            live.output.push(token);
            live.output.len() >= live.request.max_new || (live.request.stop_when)(&live.output)
        };
        if done {
            let live = self.slots[slot].take().expect("live slot");
            if let Some(backend) = self.backend.as_mut() {
                backend.release(slot);
            }
            live.request.finish(Ok(live.output));
        }
    }

    fn step(&mut self) {
        let batch: Vec<(usize, u32)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, live)| live.as_ref().map(|live| (slot, *live.output.last().expect("first token"))))
            .collect();
        if batch.is_empty() {
            return;
        }
        let backend = self.backend.as_mut().expect("backend");
        match backend.step(&batch) {
            Ok(all) => {
                for ((slot, _), logits) in batch.iter().zip(all) {
                    let token = greedy(&logits, &self.specials);
                    self.accept(*slot, token);
                }
            }
            Err(error) => {
                for (slot, _) in &batch {
                    if let Some(live) = self.slots[*slot].take() {
                        live.request.finish(Err(error.clone()));
                    }
                    backend.release(*slot);
                }
                self.last_error = Some(error);
            }
        }
    }

    /// Nothing live: drop per-sequence buffers now, the weights later.
    fn go_idle(&mut self) {
        if !self.trimmed {
            if let Some(backend) = self.backend.as_mut() {
                backend.trim();
            }
            self.trimmed = true;
            self.idle_since = Instant::now();
        }
        if self.unload_requested {
            self.unload();
            self.unload_requested = false;
        }
    }

    /// Drops the weights and the mapped prefix states; both come back from
    /// disk on the next request.
    fn unload(&mut self) {
        self.backend = None;
        self.cache.forget();
    }

    fn shutdown(mut self) {
        self.fail_all("本地模型已停止".into());
        self.backend = None;
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Logits, PrefixState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Emits `100 + position` until position `len + 3`, then the stop token.
    struct Fake {
        lens: Vec<usize>,
        steps: Arc<Mutex<Vec<usize>>>,
        trims: Arc<AtomicUsize>,
    }

    fn logits_for(next: u32) -> Logits {
        let mut logits = vec![0.0; 300];
        logits[next as usize] = 1.0;
        logits
    }

    impl Backend for Fake {
        fn device(&self) -> String {
            "fake".into()
        }
        fn capacity(&self) -> Capacity {
            Capacity { slots: 2, context: 64 }
        }
        fn state_format(&self) -> String {
            "fake".into()
        }
        fn prefix_state(&mut self, tokens: &[u32]) -> Result<PrefixState, String> {
            Ok(PrefixState { tokens: tokens.len(), format: "fake".into(), bytes: Vec::new().into() })
        }
        fn admit(&mut self, slot: usize, prefix: &PrefixState, input: &[Segment]) -> Result<Logits, String> {
            self.lens[slot] = prefix.tokens + input.iter().map(|segment| segment.len(32)).sum::<usize>();
            Ok(logits_for(100 + self.lens[slot] as u32))
        }
        fn step(&mut self, batch: &[(usize, u32)]) -> Result<Vec<Logits>, String> {
            self.steps.lock().unwrap().push(batch.len());
            Ok(batch
                .iter()
                .map(|(slot, _)| {
                    self.lens[*slot] += 1;
                    logits_for(if self.lens[*slot] >= 8 { 1 } else { 100 + self.lens[*slot] as u32 })
                })
                .collect())
        }
        fn release(&mut self, slot: usize) {
            self.lens[slot] = 0;
        }
        fn trim(&mut self) {
            self.trims.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn specials() -> Specials {
        Specials { im_start: 2, im_end: 1, end_of_text: 0, think_open: 3, think_close: 4, banned: vec![2, 3, 4], newline: vec![10] }
    }

    #[test]
    fn batches_merges_and_trims() {
        let steps = Arc::new(Mutex::new(Vec::new()));
        let trims = Arc::new(AtomicUsize::new(0));
        let (s, t) = (steps.clone(), trims.clone());
        let loader: Loader = Box::new(move || Ok(Box::new(Fake { lens: vec![0; 2], steps: s.clone(), trims: t.clone() })));
        let dir = tempfile::tempdir().unwrap();
        let scheduler = Scheduler::start(loader, specials(), Limits::default(), dir.path().to_path_buf());
        let prefix = Arc::new(vec![9u32, 9]);
        let (tx, rx) = channel();
        let never: StopWhen = Arc::new(|_| false);
        for tokens in [vec![5u32, 6], vec![7], vec![5, 6]] {
            let tx = tx.clone();
            scheduler.generate(Generate {
                prefix: prefix.clone(),
                positions: tokens.len(),
                key: format!("{tokens:?}"),
                input: Arc::new(vec![Segment::Tokens(tokens)]),
                max_new: 10,
                stop_when: never.clone(),
                lane: Lane::Foreground,
                reply: Box::new(move |result| tx.send(result).unwrap()),
            });
        }
        let mut results: Vec<Vec<u32>> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap()).collect();
        results.sort();
        // Prompt of 4 positions: tokens 104..107 then the stop token.
        assert_eq!(results[0], vec![103, 104, 105, 106, 107]);
        assert_eq!(results[1], vec![104, 105, 106, 107]);
        assert_eq!(results[2], vec![104, 105, 106, 107]); // merged duplicate
        assert!(steps.lock().unwrap().iter().any(|n| *n == 2), "two requests shared steps");
        // The thread publishes once it has gone idle.
        let started = Instant::now();
        while scheduler.status().running != 0 || trims.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "never went idle");
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = scheduler.status();
        assert!(status.loaded && !status.loading);
        assert_eq!(status.device.as_deref(), Some("fake"));
    }

    #[test]
    fn status_never_waits_for_a_load() {
        let (release, gate) = channel::<()>();
        let loader: Loader = Box::new(move || {
            gate.recv().unwrap();
            Ok(Box::new(Fake { lens: vec![0; 2], steps: Arc::default(), trims: Arc::default() }) as Box<dyn Backend>)
        });
        let dir = tempfile::tempdir().unwrap();
        let scheduler = Scheduler::start(loader, specials(), Limits::default(), dir.path().to_path_buf());
        let (seen, observed) = channel();
        scheduler.observe(Arc::new(move |status: &Status| {
            let _ = seen.send((status.loading, status.loaded));
        }));
        let (tx, rx) = channel();
        scheduler.prefix(vec![9, 9], Box::new(move |result| tx.send(result.is_ok()).unwrap()));
        // The loader is stuck; the status says so at once.
        assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), (true, false));
        let started = Instant::now();
        let status = scheduler.status();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(status.loading && !status.loaded);
        release.send(()).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), (false, true));
        assert!(scheduler.status().loaded);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let loader: Loader = Box::new(|| {
            Ok(Box::new(Fake { lens: vec![0; 2], steps: Arc::default(), trims: Arc::default() }) as Box<dyn Backend>)
        });
        let dir = tempfile::tempdir().unwrap();
        let scheduler = Scheduler::start(loader, specials(), Limits::default(), dir.path().to_path_buf());
        let prefix = Arc::new(vec![7u32; 60]);
        let (tx, rx) = channel();
        scheduler.generate(Generate {
            prefix,
            input: Arc::new(vec![Segment::Tokens(vec![1, 2, 3])]),
            positions: 3,
            key: "k".into(),
            max_new: 15,
            stop_when: Arc::new(|_| false),
            lane: Lane::Foreground,
            reply: Box::new(move |result| tx.send(result).unwrap()),
        });
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    }

    fn job(key: &str, lane: Lane, replies: &std::sync::mpsc::Sender<(String, bool)>) -> Generate {
        let (key, tx) = (key.to_string(), replies.clone());
        Generate {
            prefix: Arc::new(vec![9]),
            input: Arc::new(vec![Segment::Tokens(vec![1])]),
            positions: 1,
            key: key.clone(),
            max_new: 4,
            stop_when: Arc::new(|_| false),
            lane,
            reply: Box::new(move |result| tx.send((key, result.is_ok())).unwrap()),
        }
    }

    #[test]
    fn the_foreground_goes_first_and_the_background_gives_way() {
        let dir = tempfile::tempdir().unwrap();
        let (_sender, receiver) = channel();
        let loader: Loader = Box::new(|| Err("unused".into()));
        let limits = Limits { queue: 3, ..Limits::default() };
        let mut worker = Worker::new(loader, specials(), limits, PrefixCache::new(dir.path().to_path_buf()), receiver);
        let (tx, rx) = channel();
        worker.enqueue(job("a", Lane::Background, &tx));
        worker.enqueue(job("b", Lane::Background, &tx));
        worker.enqueue(job("c", Lane::Foreground, &tx));
        // Admitted before both subagent requests that came earlier.
        assert_eq!(worker.queue[worker.next_waiting()].key, "c");
        // A full queue: the foreground takes the newest background request's place.
        worker.enqueue(job("d", Lane::Foreground, &tx));
        assert_eq!(rx.try_recv().unwrap(), ("b".to_string(), false));
        let keys: Vec<&str> = worker.queue.iter().map(|pending| pending.key.as_str()).collect();
        assert_eq!(keys, ["a", "c", "d"]);
        // A background request finds no room; the foreground is refused only once no background is left.
        worker.enqueue(job("e", Lane::Background, &tx));
        assert_eq!(rx.try_recv().unwrap(), ("e".to_string(), false));
        worker.enqueue(job("f", Lane::Foreground, &tx));
        assert_eq!(rx.try_recv().unwrap(), ("a".to_string(), false));
        worker.enqueue(job("g", Lane::Foreground, &tx));
        assert_eq!(rx.try_recv().unwrap(), ("g".to_string(), false));
        // Asked for again by the conversation itself, a waiting background request moves up.
        let mut worker = Worker::new(
            Box::new(|| Err("unused".into())),
            specials(),
            Limits::default(),
            PrefixCache::new(dir.path().to_path_buf()),
            channel().1,
        );
        worker.enqueue(job("x", Lane::Background, &tx));
        worker.enqueue(job("y", Lane::Background, &tx));
        worker.enqueue(job("y", Lane::Foreground, &tx));
        assert_eq!(worker.queue.len(), 2);
        assert_eq!(worker.queue[worker.next_waiting()].key, "y");
    }
}
