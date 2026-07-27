//! Materialized view actor is responsible for batching and executing orders
//! while limiting the number of concurrent tasks to prevent overloading the database.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::Utc;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::{
    sync::{mpsc, watch::Receiver, Semaphore},
    time::timeout,
};
use tracing::Instrument;

use crate::{store_traits::SensorDataStore, DataKind};

// ── MatViewMsg ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatViewMsg<DC: DataKind> {
    pub data_channel: DC,
    pub data_set_id: String,
}

impl<DC: DataKind> std::hash::Hash for MatViewMsg<DC> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.data_channel.to_string().hash(state);
        self.data_set_id.hash(state);
    }
}

pub async fn start_task<DC: DataKind, DS>(
    db: DS,
    incomming_jobs: mpsc::Receiver<MatViewMsg<DC>>,
    panic_hook: PanicCallback,
    max_parallel_tasks: usize,
) -> Handle
where
    DS: SensorDataStore<DataChannel = DC> + Clone,
{
    let task_function = materialized_views_task;
    let incomming_jobs = Arc::new(tokio::sync::Mutex::new(incomming_jobs));
    Handle::new(
        move |shutdown_rx| {
            task_function(
                db.clone(),
                incomming_jobs.clone(),
                shutdown_rx,
                max_parallel_tasks,
            )
        },
        get_crate_relative_function_path(task_function),
        panic_hook,
    )
}

#[derive(Debug, Clone, PartialEq)]
struct MatViewState {
    /// Timestamp of the first message since last processing
    start_of_dataset: i64,

    /// Timestamp of the last time processing has been done for this dataset
    last_processed_ms: i64,

    /// Whether the materialized view process is currently in progress (on this server)
    in_progress: bool,

    /// Timestamp of latest message that was received for this dataset
    /// (if starting a materialized view after this time it can be evicted from the set?)
    last_received_ms: i64,
}

/// 'offline' threshold: after this short time of not receiving more data,
/// assume it is not likely for more data to follow
const THRESHOLD_RECEIVE_IDLE_MS: i64 = 3_000;

// maximum latency threshold (in case a dataset is always streaming)
const THRESHOLD_REPROCESS_MS: i64 = 60_000;

// entries older than this threshold (and which are already up-to-date) can be evicted
const THRESHOLD_STALE_MS: i64 = 8 * 3_600_000;

#[derive(Debug, Clone, Copy)]
enum MatViewNotReady {
    AlreadyBusy,
    AlreadyUpToDate,
}

impl MatViewState {
    /// Timestamp (in ms) when this state should start to be processed
    /// returns None if already in progress or already up to date
    pub fn process_at(&self) -> Result<i64, MatViewNotReady> {
        if self.in_progress {
            Err(MatViewNotReady::AlreadyBusy)
        } else {
            let t_receive_idle = self.last_received_ms + THRESHOLD_RECEIVE_IDLE_MS;

            // Already up-to-date: even if waiting for end of stream, the processing would still be up-to-date.
            if t_receive_idle <= self.last_processed_ms {
                Err(MatViewNotReady::AlreadyUpToDate)

            // Not up-to-date: to be updated at `t_receive_idle` or `t_reprocess` if that happens first
            // `t_receive_idle` will usually be first, but if the stream never goes idle, `t_reprocess` is the fallback
            } else {
                let t_reprocess = self.start_of_dataset + THRESHOLD_REPROCESS_MS;
                let deadline = t_receive_idle.min(t_reprocess);

                // rate-limit: deadline cannot be earlier than last_processed + rate limit
                let min_deadline = self.last_processed_ms + THRESHOLD_REPROCESS_MS;
                Ok(deadline.max(min_deadline))
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum NotReady {
    TryAgainAt(i64),
    QueueEmpty,
}
/// Queue of MatViewState per MatViewMsg, ordered by priority
#[derive(Debug)]
struct MatViewQueue<DC: DataKind> {
    state_map: HashMap<MatViewMsg<DC>, MatViewState>,
}

impl<DC: DataKind> MatViewQueue<DC> {
    pub fn new() -> Self {
        Self {
            state_map: HashMap::new(),
        }
    }

    /// Feedback from processing: mark task as done
    pub fn mark_done(&mut self, feedback: &MatViewMsg<DC>) {
        if let Some(state) = self.state_map.get_mut(feedback) {
            state.in_progress = false;
        }
    }

    /// Updates the last_received_ms or inserts a new entry in the queue
    pub fn update_last_received(&mut self, t_now_ms: i64, msg: &MatViewMsg<DC>) -> &MatViewState {
        // check if state exists for key
        // if it didn't exist, create it
        let s = self.state_map.entry(msg.clone()).or_insert(MatViewState {
            start_of_dataset: t_now_ms,
            last_processed_ms: 0, // placeholder for 'never'
            last_received_ms: t_now_ms,
            in_progress: false,
        });

        // prevent immediately triggering on first message since being up-to-date
        // TODO maybe MatViewState can be more typestate-like to make this nicer
        if let Err(MatViewNotReady::AlreadyUpToDate) = s.process_at() {
            s.start_of_dataset = t_now_ms;
        }

        s.last_received_ms = t_now_ms;

        // return non-mutable reference
        &*s
    }

    /// Find dataset that is most outdated and not yet in progress
    pub fn next_to_process(&mut self, t_now_ms: i64) -> Result<MatViewMsg<DC>, NotReady> {
        // collect 'stale' entries that can be evicted from the hashmap
        let mut stale = Vec::<MatViewMsg<DC>>::new();

        // find the entry with the earliest deadline
        let next: Option<(i64, (&MatViewMsg<DC>, &mut MatViewState))> =
            self.state_map.iter_mut().fold(None, |acc, candidate| {
                // Candidate must be processed: check if its deadline is before the current best one
                if let Ok(deadline) = candidate.1.process_at() {
                    match acc.as_ref().and_then(|acc| acc.1 .1.process_at().ok()) {
                        // current deadline is still most important: nothing to do
                        Some(current_deadline) if current_deadline <= deadline => acc,

                        // candidate wins: deadline is earlier (or no current deadline yet)
                        _ => Some((deadline, candidate)),
                    }

                // No deadline for this candidate
                } else {
                    // Mark entry for removal if no recent data received
                    if !candidate.1.in_progress
                        && ((t_now_ms - candidate.1.last_received_ms) > THRESHOLD_STALE_MS)
                    {
                        let x = candidate.0.clone();
                        stale.push(x);
                    }

                    acc
                }
            });

        let result = match next {
            Some((deadline, item)) => {
                // deadline in past: immediately start processing
                if deadline <= t_now_ms {
                    item.1.in_progress = true;
                    item.1.last_processed_ms = t_now_ms;

                    Ok(item.0.to_owned())

                // deadline in future
                } else {
                    Err(NotReady::TryAgainAt(deadline))
                }
            }
            None => Err(NotReady::QueueEmpty),
        };

        // garbage collect stale entries
        for obsolete in stale {
            self.state_map.remove(&obsolete);
        }

        result
    }
}

async fn materialized_views_task<DC: DataKind, DS: SensorDataStore<DataChannel = DC> + Clone>(
    db: DS,
    incomming_jobs: Arc<tokio::sync::Mutex<mpsc::Receiver<MatViewMsg<DC>>>>,
    mut shutdown_rx: Receiver<()>,
    max_parallel_tasks: usize,
) {
    tracing::info!("Starting materialized_views_task");

    let mut queue = MatViewQueue::<DC>::new();

    // For controlling the number of spawned materialized view updates
    let semaphore = Arc::new(Semaphore::new(max_parallel_tasks));
    let (feedback_tx, mut feedback_rx) = mpsc::channel::<MatViewMsg<DC>>(max_parallel_tasks);

    let mut delay_ms = 10 * 60_000;
    let mut incoming_job = incomming_jobs
        .try_lock()
        .expect("Jobs receiver seems to be locked by another task then materialized views task");

    loop {
        tokio::select! {

            // Task shutdown
            _ = shutdown_rx.changed() => {
                break;
            }

            // Delay: next scheduled job
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)) => {}

            // Feedback from running jobs
            feedback = feedback_rx.recv() => {
                match &feedback {
                    Some(fb) => queue.mark_done(fb),
                    None => break,
                }
            }

            // Incoming new jobs
            job = incoming_job.recv() => {
                match job {
                    Some(msg) => {
                        let now = Utc::now().timestamp_millis();
                        let state = queue.update_last_received(now, &msg);
                        let data_set_id = msg.data_set_id;
                        let data_channel = msg.data_channel;

                        // if it exists, check if it's already in progress.
                        if state.in_progress {
                            tracing::debug!("Materialized view for Dataset ID {data_set_id} ({data_channel}) is already in progress ({state:?})");
                        }
                    }
                    None => {
                        break;
                    }
                }
            }
        }

        // Acquire a permit for spawning a task
        while let Ok(permit) = semaphore.clone().try_acquire_owned() {
            // Create a span for this processing iteration
            let process_span = tracing::info_span!(
                "mat_view.next-queue-item",
                queue_size = queue.state_map.len()
            );
            let _process_guard = process_span.enter();

            // find dataset that is oldest (most out of sync) and not yet in progress
            let now = Utc::now().timestamp_millis();
            let msg = match queue.next_to_process(now) {
                Ok(msg) => msg,
                Err(NotReady::TryAgainAt(deadline)) => {
                    delay_ms = (deadline - now) as u64;
                    break;
                }
                Err(NotReady::QueueEmpty) => {
                    delay_ms = 10 * 60_000;
                    break;
                }
            };

            tracing::debug!(
                "Spawning a task..for Dataset ID {} ({})",
                msg.data_set_id,
                msg.data_channel
            );

            let db_clone = db.clone();
            let feedback_clone = feedback_tx.clone();
            let task_span = tracing::info_span!(
                "mat_view.process",
                data_set_id = %msg.data_set_id,
                data_channel = %msg.data_channel
            );

            tokio::spawn(
                async move {
                    if let Err(err) = db_clone
                        .update_materialized_views(msg.data_channel, &msg.data_set_id)
                        .await
                    {
                        tracing::error!(
                            "Error updating mat view: {err:?} for Dataset ID {} ({})",
                            msg.data_set_id,
                            msg.data_channel
                        );
                    }

                    // TODO maybe this can be merged in one action somehow
                    drop(permit);
                    feedback_clone.send(msg).await.ok();
                }
                .instrument(task_span),
            );
        }
    }

    let wait_for_tasks_to_finish = semaphore.acquire_many(max_parallel_tasks as u32);

    // Successful or not, we will terminate after this.
    let _ = timeout(Duration::from_secs(3 * 60), wait_for_tasks_to_finish).await;

    tracing::info!("Exit matt_view_task");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
    enum TestChannel {
        A,
        B,
    }

    impl std::fmt::Display for TestChannel {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TestChannel::A => write!(f, "A"),
                TestChannel::B => write!(f, "B"),
            }
        }
    }

    impl DataKind for TestChannel {
        fn downsampling(&self) -> bool {
            false
        }
    }

    #[test]
    fn does_not_trigger_immediately() {
        let mut queue = MatViewQueue::new();

        let msg = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };
        let t0 = 1_000_000_000;
        let state = queue.update_last_received(t0, &msg);
        assert_eq!(t0 + THRESHOLD_RECEIVE_IDLE_MS, state.process_at().unwrap());
        match queue.next_to_process(t0) {
            Err(NotReady::TryAgainAt(t)) => assert_eq!(t, t0 + THRESHOLD_RECEIVE_IDLE_MS),
            _ => panic!("queue not scheduled as expected"),
        };
    }

    #[test]
    fn does_not_trigger_untill_idle() {
        let mut queue = MatViewQueue::new();

        let msg = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };

        // insert 10 samples for the same dataset
        let t0 = 1_000_000_000;
        for i in 1..=10 {
            let state = queue.update_last_received(t0 + i, &msg);
            assert_eq!(
                t0 + THRESHOLD_RECEIVE_IDLE_MS + i,
                state.process_at().unwrap()
            );
        }

        // idle timeout should have moved forward
        match queue.next_to_process(t0) {
            Err(NotReady::TryAgainAt(t)) => assert_eq!(t, t0 + THRESHOLD_RECEIVE_IDLE_MS + 10),
            _ => panic!("queue not scheduled as expected"),
        };

        // after idle time, item should be processed
        let now = t0 + THRESHOLD_RECEIVE_IDLE_MS + 10;
        assert_eq!(msg, queue.next_to_process(now).unwrap());
        match queue.next_to_process(now) {
            Err(NotReady::QueueEmpty) => {}
            _ => panic!("queue should be empty"),
        };
    }

    #[test]
    fn triggers_even_if_never_idle() {
        let mut queue = MatViewQueue::new();

        let msg = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };

        // insert samples at a steady interval (never idle in between)
        let t0 = 1_000_000_000;
        let expect_ready = t0 + THRESHOLD_REPROCESS_MS;
        for i in (0..THRESHOLD_REPROCESS_MS).step_by((THRESHOLD_RECEIVE_IDLE_MS / 2) as usize) {
            let state = queue.update_last_received(t0 + i, &msg);
            assert!(state.process_at().unwrap() <= expect_ready);
        }

        // expecting tryagain to be scheduled at expected reprocessing interval
        match queue.next_to_process(expect_ready - 1) {
            Err(NotReady::TryAgainAt(t)) => assert_eq!(t, expect_ready),
            _ => panic!("queue not scheduled as expected"),
        };

        // reprocessing timeout: item should be processed
        assert_eq!(msg, queue.next_to_process(expect_ready).unwrap());
        match queue.next_to_process(expect_ready) {
            Err(NotReady::QueueEmpty) => {}
            _ => panic!("queue should be empty"),
        };
    }

    #[test]
    fn does_not_retrigger_untill_rate_limit_after_first_done() {
        let mut queue = MatViewQueue::new();

        let msg = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };

        // insert 10 samples for the same dataset
        let t0 = 1_000_000_000;
        let mut now = t0;
        for _ in 0..10 {
            now += 1;
            let _state = queue.update_last_received(now, &msg);
        }

        // 1. after idle time, item is marked as 'busy' and should be processed
        now += THRESHOLD_RECEIVE_IDLE_MS;
        let t_start_process = now;
        assert_eq!(msg, queue.next_to_process(now).unwrap());
        match queue.next_to_process(now) {
            Err(NotReady::QueueEmpty) => {}
            _ => panic!("queue should be empty"),
        };

        // 2. insert some more samples for the same dataset
        for _ in 0..10 {
            now += 1;
            let _state = queue.update_last_received(now, &msg);
        }

        // expect QueueEmpty because the item is still in progress
        match queue.next_to_process(now) {
            Err(NotReady::QueueEmpty) => {}
            _ => panic!("queue should be empty"),
        };

        // 3. mark processing as done. this confirms the processing in step 1
        // and unlocks future processing for this dataset
        queue.mark_done(&msg);

        // 4. the data from step 2 can be processed after rate-limit relative to previous processing again.
        match queue.next_to_process(now) {
            Err(NotReady::TryAgainAt(t)) => {
                assert_eq!(t, t_start_process + THRESHOLD_REPROCESS_MS);
            }
            other => panic!("queue should not be empty{other:?}"),
        };

        // .. some time later: process everything
        now += 2 * THRESHOLD_REPROCESS_MS;
        assert_eq!(msg, queue.next_to_process(now).unwrap());
        queue.mark_done(&msg);

        // start a new 'sync session' of 1 sample
        now += 20 * THRESHOLD_REPROCESS_MS;
        let _state = queue.update_last_received(now, &msg);

        match queue.next_to_process(now) {
            Err(NotReady::TryAgainAt(t)) => {
                assert_eq!(t, now + THRESHOLD_RECEIVE_IDLE_MS);
            }
            other => panic!("queue should not be empty{other:?}"),
        };
        now += THRESHOLD_RECEIVE_IDLE_MS;
        assert_eq!(msg, queue.next_to_process(now).unwrap());
    }

    #[test]
    fn stale_item_evicted() {
        let mut queue = MatViewQueue::new();

        let msg = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };
        let t0 = 1_000_000_000;

        // 1. insert a message
        let state = queue.update_last_received(t0, &msg);
        assert_eq!(t0 + THRESHOLD_RECEIVE_IDLE_MS, state.process_at().unwrap());

        // 2. process it and mark done
        queue
            .next_to_process(t0 + THRESHOLD_RECEIVE_IDLE_MS)
            .unwrap();
        queue.mark_done(&msg);

        // 3. item should still be in map
        assert_eq!(1, queue.state_map.len());

        // 4. item should be evicted after long time
        queue
            .next_to_process(t0 + THRESHOLD_STALE_MS + 1)
            .unwrap_err();
        assert_eq!(0, queue.state_map.len());
    }

    #[test]
    fn different_items_deadline_order_is_arrival_order() {
        let mut queue = MatViewQueue::new();

        let msg1 = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };
        let msg2 = MatViewMsg {
            data_channel: TestChannel::B,
            data_set_id: "test".into(),
        };

        let t0 = 1_000_000_000;
        let mut now = t0;
        queue.update_last_received(now, &msg1);
        now += 1;
        queue.update_last_received(now, &msg2);

        // after some idle time: both messages sorted by priority (=in order of arrival)
        now += THRESHOLD_RECEIVE_IDLE_MS;
        assert_eq!(msg1, queue.next_to_process(now).unwrap());
        assert_eq!(msg2, queue.next_to_process(now).unwrap());
        queue.next_to_process(now).unwrap_err();
        queue.mark_done(&msg2);
        queue.mark_done(&msg1);
        queue.next_to_process(now).unwrap_err();
    }

    #[test]
    fn different_items_deadline_order_when_ratelimiting() {
        let mut queue = MatViewQueue::new();

        let msg1 = MatViewMsg {
            data_channel: TestChannel::A,
            data_set_id: "test".into(),
        };
        let msg2 = MatViewMsg {
            data_channel: TestChannel::B,
            data_set_id: "test".into(),
        };

        let t0 = 1_000_000_000;
        let mut now = t0;
        // msg1 arrives at t0, msg2 100ms later
        queue.update_last_received(now, &msg1);
        now += 100;
        queue.update_last_received(now, &msg2);

        // process msg1 as soon as it is idle
        now = t0 + THRESHOLD_RECEIVE_IDLE_MS;
        assert_eq!(msg1, queue.next_to_process(now).unwrap());
        queue.next_to_process(now).unwrap_err();

        // 100ms later msg2 is also idle
        now += 100;
        assert_eq!(msg2, queue.next_to_process(now).unwrap());
        queue.next_to_process(now).unwrap_err();
        queue.mark_done(&msg2);
        queue.mark_done(&msg1);

        // msg 2 and 1 arrive again in reverse order.
        // this should not have impact within THRESHOLD_REPROCESS_MS after the previous processing
        now += 10;
        queue.update_last_received(now, &msg2);
        now += 10;
        queue.update_last_received(now, &msg1);

        // msg1 is ready first, even though msg2 was received earlier
        let next_msg1 = t0 + THRESHOLD_RECEIVE_IDLE_MS + THRESHOLD_REPROCESS_MS;
        assert_eq!(msg1, queue.next_to_process(next_msg1).unwrap());
        queue.next_to_process(now).unwrap_err();

        let next_msg2 = next_msg1 + 100;
        assert_eq!(msg2, queue.next_to_process(next_msg2).unwrap());
        queue.next_to_process(now).unwrap_err();
    }
}
