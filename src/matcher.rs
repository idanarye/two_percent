use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};

use defer_drop::DeferDrop;
use rayon::ThreadPool;

use tuikit::key::Key;

use crate::event::Event;
use crate::item::{ItemPool, MatchedItem};
use crate::spinlock::SpinLock;
use crate::{CaseMatching, MatchEngineFactory, MatchResult};
use crate::{MatchRange, Rank};
use std::rc::Rc;

use hashbrown::HashMap;
use nohash::NoHashHasher;
use std::hash::BuildHasherDefault;

#[cfg(feature = "malloc_trim")]
#[cfg(target_os = "linux")]
#[cfg(target_env = "gnu")]
use crate::malloc_trim;

const UNMATCHED_RANK: Rank = [0i32, 0i32, 0i32, 0i32];
const UNMATCHED_RANGE: Option<MatchRange> = None;

//==============================================================================
pub struct MatcherControl {
    stopped: Arc<AtomicBool>,
    processed: Arc<AtomicUsize>,
    matched: Arc<AtomicUsize>,
    items: Arc<SpinLock<Vec<MatchedItem>>>,
    opt_thread_handle: Option<JoinHandle<()>>,
}

impl Drop for MatcherControl {
    fn drop(&mut self) {
        self.kill();
        // lock before drop
        drop(self.take());
    }
}

impl MatcherControl {
    pub fn get_num_processed(&self) -> usize {
        self.processed.load(Ordering::Relaxed)
    }

    pub fn get_num_matched(&self) -> usize {
        self.matched.load(Ordering::Relaxed)
    }

    pub fn kill(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(handle) = self.opt_thread_handle.take() {
            let _ = handle.join();
            #[cfg(feature = "malloc_trim")]
            #[cfg(target_os = "linux")]
            #[cfg(target_env = "gnu")]
            malloc_trim()
        }
    }

    pub fn take(&mut self) -> Vec<MatchedItem> {
        let mut items = self.items.lock();
        std::mem::take(&mut *items)
    }

    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn into_items(&mut self) -> Vec<MatchedItem> {
        while !self.stopped.load(Ordering::Relaxed) {}
        let mut locked = self.items.lock();

        std::mem::take(&mut *locked)
    }
}

//==============================================================================
pub struct Matcher {
    engine_factory: Rc<dyn MatchEngineFactory>,
    case_matching: CaseMatching,
}

impl Matcher {
    pub fn builder(engine_factory: Rc<dyn MatchEngineFactory>) -> Self {
        Self {
            engine_factory,
            case_matching: CaseMatching::default(),
        }
    }

    pub fn case(mut self, case_matching: CaseMatching) -> Self {
        self.case_matching = case_matching;
        self
    }

    pub fn build(self) -> Self {
        self
    }

    pub fn run(
        &self,
        query: &str,
        disabled: bool,
        thread_pool_weak: Weak<ThreadPool>,
        item_pool_weak: Weak<DeferDrop<ItemPool>>,
        tx_heartbeat: Sender<(Key, Event)>,
        matched_items: Vec<MatchedItem>,
    ) -> MatcherControl {
        let matcher_engine = self.engine_factory.create_engine_with_case(query, self.case_matching);
        debug!("engine: {}", matcher_engine);
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_clone = stopped.clone();
        let processed = Arc::new(AtomicUsize::new(0));
        let processed_clone = processed.clone();
        let matched = Arc::new(AtomicUsize::new(0));
        let matched_clone = matched.clone();
        let matched_items = Arc::new(SpinLock::new(matched_items));
        let matched_items_weak = Arc::downgrade(&matched_items);

        // shortcut for when there is no query or query is disabled
        let matcher_disabled: bool = disabled || query.is_empty();

        let matcher_handle = thread::spawn(move || {
            if let Some(thread_pool_strong) = thread_pool_weak.upgrade() {
                thread_pool_strong.install(|| {
                    if let Some(item_pool_strong) = Weak::upgrade(&item_pool_weak) {
                        let num_taken = item_pool_strong.num_taken();
                        let items = item_pool_strong.take();
                        let stopped_ref = stopped.as_ref();
                        let processed_ref = processed.as_ref();
                        let matched_ref = matched.as_ref();

                        trace!("matcher start, total: {}", items.len());

                        if let Some(matched_items_strong) = Weak::upgrade(&matched_items_weak) {
                            let mut group_map: HashMap<
                                u64,
                                (Vec<usize>, Option<Option<MatchResult>>),
                                BuildHasherDefault<NoHashHasher<u64>>,
                            > = HashMap::with_capacity_and_hasher(8192, BuildHasherDefault::default());

                            items.iter().enumerate().for_each(|(idx, item)| {
                                let key = std::ptr::addr_of!(item) as u64;

                                match group_map.get_mut(&key) {
                                    Some(values) => {
                                        values.0.push(idx);
                                    }
                                    None => {
                                        let _ = group_map.insert_unique_unchecked(key, (vec![idx], None));
                                    }
                                }
                            });

                            let par_iter = items
                                .iter()
                                .enumerate()
                                .take_while(|_| {
                                    if stopped_ref.load(Ordering::Relaxed) {
                                        return false;
                                    }

                                    processed_ref.fetch_add(1, Ordering::Relaxed);
                                    true
                                })
                                .filter_map(|(index, item)| {
                                    // dummy values should not change, as changing them
                                    // may cause the disabled/query empty case disappear!
                                    // especially item index.  Needs an index to appear!

                                    let key = std::ptr::addr_of!(item) as u64;

                                    group_map
                                        .get_mut(&key)
                                        .and_then(|values| match &values.1 {
                                            Some(res) => res.to_owned(),
                                            None => matcher_engine.match_item(item.as_ref()),
                                        })
                                        .map(|res| (index, res, item))
                                })
                                .map(|(index, res, item)| {
                                    if matcher_disabled {
                                        return MatchedItem {
                                            item: Arc::downgrade(item),
                                            rank: UNMATCHED_RANK,
                                            matched_range: UNMATCHED_RANGE,
                                            item_idx: (num_taken + index) as u32,
                                        };
                                    }

                                    matched_ref.fetch_add(1, Ordering::Relaxed);

                                    MatchedItem {
                                        item: Arc::downgrade(item),
                                        rank: res.rank,
                                        matched_range: Some(res.matched_range),
                                        item_idx: (num_taken + index) as u32,
                                    }
                                });

                            if !stopped_ref.load(Ordering::Relaxed) {
                                let mut pool = matched_items_strong.lock();
                                pool.clear();
                                pool.extend(par_iter);
                                trace!("matcher stop, total matched: {}", pool.len());
                            }
                        }
                    }
                });
            }

            let _ = tx_heartbeat.send((Key::Null, Event::EvHeartBeat));
            stopped.store(true, Ordering::Relaxed);
        });

        MatcherControl {
            stopped: stopped_clone,
            matched: matched_clone,
            processed: processed_clone,
            items: matched_items,
            opt_thread_handle: Some(matcher_handle),
        }
    }
}
