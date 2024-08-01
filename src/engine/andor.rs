use std::fmt::{Display, Error, Formatter};

use crate::{MatchEngine, MatchRange, MatchResult, SkimItem};

//------------------------------------------------------------------------------
// OrEngine, a combinator
pub struct OrEngine<T: SkimItem> {
    engines: Vec<Box<dyn MatchEngine<T>>>,
}

impl<T: SkimItem> OrEngine<T> {
    pub fn builder() -> Self {
        Self { engines: vec![] }
    }

    pub fn engines(mut self, mut engines: Vec<Box<dyn MatchEngine<T>>>) -> Self {
        self.engines.append(&mut engines);
        self
    }

    pub fn build(self) -> Self {
        self
    }
}

impl<T: SkimItem> MatchEngine<T> for OrEngine<T> {
    fn match_item(&self, item: &T) -> Option<MatchResult> {
        self.engines.iter().find_map(|engine| engine.match_item(item))
    }
}

impl<T: SkimItem> Display for OrEngine<T> {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        write!(
            f,
            "(Or: {})",
            self.engines
                .iter()
                .map(|e| format!("{}", e))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

//------------------------------------------------------------------------------
// AndEngine, a combinator
pub struct AndEngine<T: SkimItem> {
    engines: Vec<Box<dyn MatchEngine<T>>>,
}

impl<T: SkimItem> AndEngine<T> {
    pub fn builder() -> Self {
        Self { engines: vec![] }
    }

    pub fn engines(mut self, mut engines: Vec<Box<dyn MatchEngine<T>>>) -> Self {
        self.engines.append(&mut engines);
        self
    }

    pub fn build(self) -> Self {
        self
    }

    fn merge_matched_items(&self, items: Vec<MatchResult>, text: &str) -> MatchResult {
        let rank = items[0].rank;
        let mut ranges = vec![];
        for item in items {
            match item.matched_range {
                MatchRange::ByteRange(..) => {
                    ranges.extend(item.range_char_indices(text));
                }
                MatchRange::Chars(vec) => {
                    ranges.extend(vec.iter());
                }
            }
        }

        ranges.sort();
        ranges.dedup();
        MatchResult {
            rank,
            matched_range: MatchRange::Chars(ranges.into()),
        }
    }
}

impl<T: SkimItem> MatchEngine<T> for AndEngine<T> {
    fn match_item(&self, item: &T) -> Option<MatchResult> {
        // mock
        let mut results = vec![];
        for engine in &self.engines {
            let result = engine.match_item(item)?;
            results.push(result);
        }

        if results.is_empty() {
            None
        } else {
            Some(self.merge_matched_items(results, &item.text()))
        }
    }
}

impl<T: SkimItem> Display for AndEngine<T> {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        write!(
            f,
            "(And: {})",
            self.engines
                .iter()
                .map(|e| format!("{}", e))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
