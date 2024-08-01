///! An item is line of text that read from `find` command or stdin together with
///! the internal states, such as selected or not
use std::cmp::min;
use std::default::Default;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use crate::spinlock::{SpinLock, SpinLockGuard};
use crate::{MatchRange, Rank, SkimItem};

//------------------------------------------------------------------------------

#[derive(Debug)]
pub struct RankBuilder {
    criterion: Vec<RankCriteria>,
}

impl Default for RankBuilder {
    fn default() -> Self {
        Self {
            criterion: vec![RankCriteria::Score, RankCriteria::Begin, RankCriteria::End],
        }
    }
}

impl RankBuilder {
    pub fn new(mut criterion: Vec<RankCriteria>) -> Self {
        if !criterion.contains(&RankCriteria::Score) && !criterion.contains(&RankCriteria::NegScore) {
            criterion.insert(0, RankCriteria::Score);
        }

        criterion.dedup();
        Self { criterion }
    }

    /// score: the greater the better
    pub fn build_rank(&self, score: i32, begin: usize, end: usize, length: usize) -> Rank {
        let mut rank = [0; 4];
        let begin = begin as i32;
        let end = end as i32;
        let length = length as i32;

        for (index, criteria) in self.criterion.iter().take(4).enumerate() {
            let value = match criteria {
                RankCriteria::Score => -score,
                RankCriteria::Begin => begin,
                RankCriteria::End => end,
                RankCriteria::NegScore => score,
                RankCriteria::NegBegin => -begin,
                RankCriteria::NegEnd => -end,
                RankCriteria::Length => length,
                RankCriteria::NegLength => -length,
            };

            rank[index] = value;
        }

        rank
    }
}

//------------------------------------------------------------------------------
pub struct MatchedItem<T: SkimItem> {
    pub item: Weak<T>,
    pub rank: Rank,
    pub matched_range: Option<MatchRange>, // range of chars that matched the pattern
    pub item_idx: u32,
}

impl<T: SkimItem> Clone for MatchedItem<T> {
    fn clone(&self) -> Self {
        Self {
            item: self.item.clone(),
            rank: self.rank,
            matched_range: self.matched_range.clone(),
            item_idx: self.item_idx,
        }
    }
}

use std::cmp::Ordering as CmpOrd;

impl<T: SkimItem> PartialEq for MatchedItem<T> {
    fn eq(&self, other: &Self) -> bool {
        self.rank.eq(&other.rank)
    }
}

impl<T: SkimItem> std::cmp::Eq for MatchedItem<T> {}

impl<T: SkimItem> PartialOrd for MatchedItem<T> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrd> {
        self.rank.partial_cmp(&other.rank)
    }
}

impl<T: SkimItem> Ord for MatchedItem<T> {
    fn cmp(&self, other: &Self) -> CmpOrd {
        self.rank.cmp(&other.rank)
    }
}

//------------------------------------------------------------------------------
const ITEM_POOL_CAPACITY: usize = 1024;

pub struct ItemPool<T: SkimItem> {
    length: AtomicUsize,
    pool: SpinLock<Vec<Arc<T>>>,
    /// number of items that was `take`n
    taken: AtomicUsize,

    /// reverse first N lines as header
    reserved_items: SpinLock<Vec<Weak<T>>>,
    lines_to_reserve: usize,
}

impl<T: SkimItem> Drop for ItemPool<T> {
    fn drop(&mut self) {
        let mut pool = self.pool.lock();
        let mut reserved_items = self.reserved_items.lock();

        std::mem::take(&mut *pool);
        std::mem::take(&mut *reserved_items);
    }
}

impl<T: SkimItem> Default for ItemPool<T> {
    fn default() -> Self {
        ItemPool::new()
    }
}

impl<T: SkimItem> ItemPool<T> {
    pub fn new() -> Self {
        Self {
            length: AtomicUsize::new(0),
            pool: SpinLock::new(Vec::with_capacity(ITEM_POOL_CAPACITY)),
            taken: AtomicUsize::new(0),
            reserved_items: SpinLock::new(Vec::new()),
            lines_to_reserve: 0,
        }
    }

    pub fn lines_to_reserve(mut self, lines_to_reserve: usize) -> Self {
        self.lines_to_reserve = lines_to_reserve;
        self
    }

    pub fn len(&self) -> usize {
        self.length.load(Ordering::SeqCst)
    }

    pub fn num_not_taken(&self) -> usize {
        self.length.load(Ordering::SeqCst) - self.taken.load(Ordering::SeqCst)
    }

    pub fn num_taken(&self) -> usize {
        self.taken.load(Ordering::SeqCst)
    }

    pub fn clear(&self) {
        let mut items = self.pool.lock();
        items.clear();
        let mut header_items = self.reserved_items.lock();
        header_items.clear();
        self.taken.store(0, Ordering::SeqCst);
        self.length.store(0, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        // lock to ensure consistency
        let _items = self.pool.lock();
        self.taken.store(0, Ordering::SeqCst);
    }

    /// append the items and return the new_size of the pool
    pub fn append(&self, mut items: Vec<Arc<dyn SkimItem>>) -> usize {
        let len = items.len();
        trace!("item pool, append {} items", len);
        let mut pool = self.pool.lock();
        let mut header_items = self.reserved_items.lock();

        let to_reserve = self.lines_to_reserve - header_items.len();
        if to_reserve > 0 {
            let to_reserve = min(to_reserve, items.len());
            let reserved_pool: &[Arc<dyn SkimItem>] = &items[..to_reserve];
            pool.extend_from_slice(reserved_pool);
            let mut reserved_header: Vec<Weak<dyn SkimItem>> = reserved_pool.iter().map(Arc::downgrade).collect();
            header_items.append(&mut reserved_header);
        } else {
            pool.append(&mut items);
        }
        self.length.store(pool.len(), Ordering::SeqCst);
        trace!("item pool, done append {} items", len);
        pool.len()
    }

    pub fn take(&self) -> ItemPoolGuard<Arc<dyn SkimItem>> {
        let guard = self.pool.lock();
        let taken = self.taken.swap(guard.len(), Ordering::SeqCst);
        ItemPoolGuard { guard, start: taken }
    }

    pub fn reserved(&self) -> ItemPoolGuard<Weak<dyn SkimItem>> {
        let guard = self.reserved_items.lock();
        ItemPoolGuard { guard, start: 0 }
    }
}

pub struct ItemPoolGuard<'a, T: Sized + 'a> {
    guard: SpinLockGuard<'a, Vec<T>>,
    start: usize,
}

impl<'mutex, T: Sized> Deref for ItemPoolGuard<'mutex, T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.guard[self.start..]
    }
}

//------------------------------------------------------------------------------
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RankCriteria {
    Score,
    Begin,
    End,
    NegScore,
    NegBegin,
    NegEnd,
    Length,
    NegLength,
}

pub fn parse_criteria(text: &str) -> Option<RankCriteria> {
    match text.to_lowercase().as_ref() {
        "score" => Some(RankCriteria::Score),
        "begin" => Some(RankCriteria::Begin),
        "end" => Some(RankCriteria::End),
        "-score" => Some(RankCriteria::NegScore),
        "-begin" => Some(RankCriteria::NegBegin),
        "-end" => Some(RankCriteria::NegEnd),
        "length" => Some(RankCriteria::Length),
        "-length" => Some(RankCriteria::NegLength),
        _ => None,
    }
}
