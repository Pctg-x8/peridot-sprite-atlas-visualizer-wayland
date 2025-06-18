use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use crossbeam::{
    channel::TryRecvError,
    deque::{Injector, Worker},
};

use crate::platform::linux::{EventFD, EventFDOptions};

pub enum BackgroundWork {
    LoadSpriteSource(PathBuf, Box<dyn FnMut(PathBuf, image::DynamicImage) + Send>),
}

pub enum BackgroundWorkerViewFeedback {
    BeginWork(usize, String),
    EndWork(usize),
}

#[derive(Clone)]
pub struct BackgroundWorkerEnqueueAccess(Arc<Injector<BackgroundWork>>);
impl BackgroundWorkerEnqueueAccess {
    #[inline]
    pub fn enqueue(&self, work: BackgroundWork) {
        self.0.push(work);
    }

    #[inline]
    pub fn downgrade(&self) -> BackgroundWorkerEnqueueWeakAccess {
        BackgroundWorkerEnqueueWeakAccess(Arc::downgrade(&self.0))
    }
}

#[derive(Clone)]
pub struct BackgroundWorkerEnqueueWeakAccess(std::sync::Weak<Injector<BackgroundWork>>);
impl BackgroundWorkerEnqueueWeakAccess {
    #[inline]
    pub fn upgrade(&self) -> Option<BackgroundWorkerEnqueueAccess> {
        self.0.upgrade().map(BackgroundWorkerEnqueueAccess)
    }
}

pub struct BackgroundWorker {
    join_handles: Vec<JoinHandle<()>>,
    work_queue: Arc<Injector<BackgroundWork>>,
    teardown_signal: Arc<AtomicBool>,
    view_feedback_receiver: crossbeam::channel::Receiver<BackgroundWorkerViewFeedback>,
    view_feedback_notifier: Arc<EventFD>,
}
impl BackgroundWorker {
    pub fn new() -> Self {
        let worker_count = std::thread::available_parallelism().map_or(4, core::num::NonZero::get);
        let work_queue = Injector::new();
        let (mut join_handles, mut local_queues, mut stealers) = (
            Vec::with_capacity(worker_count),
            Vec::with_capacity(worker_count),
            Vec::with_capacity(worker_count),
        );
        for _ in 0..worker_count {
            let local_queue = Worker::new_fifo();
            stealers.push(local_queue.stealer());
            local_queues.push(local_queue);
        }
        let stealers = Arc::new(stealers);
        let work_queue = Arc::new(work_queue);
        let teardown_signal = Arc::new(AtomicBool::new(false));
        let (view_feedback_sender, view_feedback_receiver) = crossbeam::channel::unbounded();
        let view_feedback_notifier =
            Arc::new(EventFD::new(0, EventFDOptions::CLOEXEC | EventFDOptions::NONBLOCK).unwrap());
        for (n, local_queue) in local_queues.into_iter().enumerate() {
            join_handles.push(
                std::thread::Builder::new()
                    .name(format!("Background Worker #{}", n + 1))
                    .spawn({
                        let stealers = stealers.clone();
                        let work_queue = work_queue.clone();
                        let teardown_signal = teardown_signal.clone();
                        let view_feedback_sender = view_feedback_sender.clone();
                        let view_feedback_notifier = view_feedback_notifier.clone();

                        move || {
                            while !teardown_signal.load(Ordering::Acquire) {
                                let next = local_queue.pop().or_else(|| {
                                    core::iter::repeat_with(|| {
                                        work_queue.steal_batch_and_pop(&local_queue).or_else(|| {
                                            stealers.iter().map(|x| x.steal()).collect()
                                        })
                                    })
                                    .find(|x| !x.is_retry())
                                    .and_then(|x| x.success())
                                });

                                match next {
                                    Some(BackgroundWork::LoadSpriteSource(path, mut on_complete)) => {
                                        match view_feedback_sender.send(BackgroundWorkerViewFeedback::BeginWork(n, format!("Loading {}", path.display()))) {
                                            Ok(()) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "sending view feedback failed");
                                            }
                                        }
                                        match view_feedback_notifier.add(1) {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "notifying view feedback failed");
                                            }
                                        }

                                        let img = image::open(&path).unwrap();
                                        on_complete(path, img);

                                        match view_feedback_sender.send(BackgroundWorkerViewFeedback::EndWork(n)) {
                                            Ok(()) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "sending view feedback failed");
                                            }
                                        }
                                        match view_feedback_notifier.add(1) {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "notifying view feedback failed");
                                            }
                                        }
                                    }
                                    None => {
                                        // wait for new event
                                        // TODO: 一旦sleep(1)する（本当はparkとかしてあげたほうがいい）
                                        std::thread::yield_now();
                                    }
                                }
                            }
                        }
                    })
                    .unwrap(),
            );
        }

        tracing::info!(parallelism = worker_count, "BackgroundWorker initialized");

        Self {
            join_handles,
            work_queue,
            teardown_signal,
            view_feedback_receiver,
            view_feedback_notifier,
        }
    }

    #[inline(always)]
    pub fn worker_count(&self) -> usize {
        self.join_handles.len()
    }

    #[inline(always)]
    pub fn view_feedback_notifier(&self) -> &EventFD {
        &self.view_feedback_notifier
    }

    #[inline(always)]
    pub fn clear_view_feedback_notification(&self) -> std::io::Result<()> {
        self.view_feedback_notifier.take().map(drop)
    }

    #[inline]
    pub fn try_pop_view_feedback(&self) -> Option<BackgroundWorkerViewFeedback> {
        match self.view_feedback_receiver.try_recv() {
            Ok(x) => Some(x),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                tracing::warn!("BackgroundWorker View Feedback channel has disconnected");

                None
            }
        }
    }

    #[inline(always)]
    pub fn enqueue_access(&self) -> BackgroundWorkerEnqueueAccess {
        BackgroundWorkerEnqueueAccess(self.work_queue.clone())
    }

    pub fn teardown(self) {
        self.teardown_signal.store(true, Ordering::Release);
        for x in self.join_handles {
            x.join().unwrap();
        }
    }
}
