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

pub enum BackgroundWork<'subsystem> {
    LoadSpriteSource(
        PathBuf,
        Box<dyn FnMut(PathBuf, image::DynamicImage) + Send + 'subsystem>,
    ),
}

pub enum BackgroundWorkerViewFeedback {
    BeginWork(usize, String),
    EndWork(usize),
}

#[derive(Clone)]
pub struct BackgroundWorkerEnqueueAccess<'subsystem>(Arc<Injector<BackgroundWork<'subsystem>>>);
impl<'subsystem> BackgroundWorkerEnqueueAccess<'subsystem> {
    #[inline]
    pub fn enqueue(&self, work: BackgroundWork<'subsystem>) {
        self.0.push(work);
    }

    #[inline]
    pub fn downgrade(&self) -> BackgroundWorkerEnqueueWeakAccess<'subsystem> {
        BackgroundWorkerEnqueueWeakAccess(Arc::downgrade(&self.0))
    }
}

#[derive(Clone)]
pub struct BackgroundWorkerEnqueueWeakAccess<'subsystem>(
    std::sync::Weak<Injector<BackgroundWork<'subsystem>>>,
);
impl<'subsystem> BackgroundWorkerEnqueueWeakAccess<'subsystem> {
    #[inline]
    pub fn upgrade(&self) -> Option<BackgroundWorkerEnqueueAccess<'subsystem>> {
        self.0.upgrade().map(BackgroundWorkerEnqueueAccess)
    }
}

pub struct BackgroundWorker<'subsystem> {
    join_handles: Vec<JoinHandle<()>>,
    work_queue: Arc<Injector<BackgroundWork<'subsystem>>>,
    teardown_signal: Arc<AtomicBool>,
    view_feedback_receiver: crossbeam::channel::Receiver<BackgroundWorkerViewFeedback>,
    #[cfg(target_os = "linux")]
    main_thread_waker: Arc<crate::platform::linux::EventFD>,
    #[cfg(windows)]
    main_thread_waker: Arc<crate::platform::win32::event::EventObject>,
}
impl<'subsystem> BackgroundWorker<'subsystem> {
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
        #[cfg(target_os = "linux")]
        let main_thread_waker = Arc::new(
            crate::platform::linux::EventFD::new(
                0,
                crate::platform::linux::EventFDOptions::CLOEXEC
                    | crate::platform::linux::EventFDOptions::NONBLOCK,
            )
            .unwrap(),
        );
        #[cfg(windows)]
        let main_thread_waker =
            Arc::new(crate::platform::win32::event::EventObject::new(None, true, false).unwrap());
        for (n, local_queue) in local_queues.into_iter().enumerate() {
            join_handles.push(
                unsafe {std::thread::Builder::new()
                    .name(format!("Background Worker #{}", n + 1))
                    .spawn_unchecked({
                        let stealers = stealers.clone();
                        let work_queue = work_queue.clone();
                        let teardown_signal = teardown_signal.clone();
                        let view_feedback_sender = view_feedback_sender.clone();
                        let main_thread_waker = main_thread_waker.clone();

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
                                        #[cfg(target_os = "linux")]
                                        match main_thread_waker.add(1) {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "waking main thread failed");
                                            }
                                        }
                                        #[cfg(windows)]
                                        match main_thread_waker.set() {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "waking main thread failed");
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
                                        #[cfg(target_os = "linux")]
                                        match main_thread_waker.add(1) {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "waking main thread failed");
                                            }
                                        }
                                        #[cfg(windows)]
                                        match main_thread_waker.set() {
                                            Ok(_) => (),
                                            Err(e) => {
                                                tracing::warn!(reason = ?e, "waking main thread failed");
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
                    .unwrap()},
            );
        }

        tracing::info!(parallelism = worker_count, "BackgroundWorker initialized");

        Self {
            join_handles,
            work_queue,
            teardown_signal,
            view_feedback_receiver,
            main_thread_waker,
        }
    }

    #[cfg(target_os = "linux")]
    #[inline(always)]
    pub fn main_thread_waker(&self) -> &crate::platform::linux::EventFD {
        &self.main_thread_waker
    }

    #[cfg(windows)]
    #[inline(always)]
    pub fn main_thread_waker(&self) -> &crate::platform::win32::event::EventObject {
        &self.main_thread_waker
    }

    #[inline(always)]
    pub fn clear_view_feedback_notification(&self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.main_thread_waker.take().map(drop)
        }
        #[cfg(windows)]
        {
            self.main_thread_waker.reset().map_err(From::from)
        }
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
    pub fn enqueue_access(&self) -> BackgroundWorkerEnqueueAccess<'subsystem> {
        BackgroundWorkerEnqueueAccess(self.work_queue.clone())
    }

    pub fn teardown(self) {
        self.teardown_signal.store(true, Ordering::Release);
        for x in self.join_handles {
            x.join().unwrap();
        }
    }
}
