/// helper for turn a BufRead into a skim stream
use std::io::BufRead;
use std::sync::Arc;

use crossbeam_channel::{SendError, Sender};
use regex::Regex;

use crate::field::FieldRange;
use crate::model::BACKGROUND_THREAD_POOL;
use crate::SkimItem;
use hashbrown::HashMap;
use nohash::NoHashHasher;
use std::hash::BuildHasherDefault;
use std::io::ErrorKind;
use std::sync::Weak;

#[cfg(feature = "malloc_trim")]
#[cfg(target_os = "linux")]
#[cfg(target_env = "gnu")]
use crate::malloc_trim;

use super::item::DefaultSkimItem;

#[derive(Clone)]
pub enum SendRawOrBuild<'a> {
    Raw,
    Build(BuildOptions<'a>),
}

#[derive(Clone)]
pub struct BuildOptions<'a> {
    pub ansi_enabled: bool,
    pub trans_fields: &'a [FieldRange],
    pub matching_fields: &'a [FieldRange],
    pub delimiter: &'a Regex,
}

#[allow(unused_assignments)]
pub fn ingest_loop(
    mut source: Box<dyn BufRead + Send>,
    line_ending: u8,
    tx_item: Sender<Arc<dyn SkimItem>>,
    opts: SendRawOrBuild,
) {
    let mut bytes_buffer = Vec::with_capacity(65_536);

    let mut string_intern: HashMap<u64, Weak<dyn SkimItem>, BuildHasherDefault<NoHashHasher<u64>>> =
        HashMap::with_capacity_and_hasher(8192, BuildHasherDefault::default());

    loop {
        // first, read lots of bytes into the buffer
        match source.fill_buf() {
            Ok(res) => {
                bytes_buffer.extend_from_slice(res);
                source.consume(bytes_buffer.len());
            }
            Err(err) => match err.kind() {
                ErrorKind::Interrupted => continue,
                ErrorKind::UnexpectedEof | _ => {
                    break;
                }
            },
        }

        // now, keep reading to make sure we haven't stopped in the middle of a word.
        // no need to add the bytes to the total buf_len, as these bytes are auto-"consumed()",
        // and bytes_buffer will be extended from slice to accommodate the new bytes
        let _ = source.read_until(line_ending, &mut bytes_buffer);

        // break when there is nothing left to read
        if bytes_buffer.is_empty() {
            break;
        }

        if let Err(_err) = std::str::from_utf8_mut(&mut bytes_buffer)
            .expect("Could not convert bytes to valid UTF8.")
            .lines()
            .try_for_each(|line| send(line, &opts, &tx_item, &mut string_intern))
        {
            break;
        }

        bytes_buffer.clear();
    }

    BACKGROUND_THREAD_POOL.spawn(|| {
        drop(string_intern);

        #[cfg(feature = "malloc_trim")]
        #[cfg(target_os = "linux")]
        #[cfg(target_env = "gnu")]
        malloc_trim();
    })
}

fn send(
    line: &str,
    opts: &SendRawOrBuild,
    tx_item: &Sender<Arc<dyn SkimItem>>,
    string_intern: &mut HashMap<u64, Weak<dyn SkimItem>, BuildHasherDefault<NoHashHasher<u64>>>,
) -> Result<(), SendError<Arc<dyn SkimItem>>> {
    let key = hash(&line.as_bytes());

    match string_intern.get(&key).and_then(|value| Weak::upgrade(value)) {
        Some(value) => tx_item.send(value),
        None => {
            let item: Arc<dyn SkimItem> = match opts {
                SendRawOrBuild::Build(opts) => {
                    let item = DefaultSkimItem::new(
                        line,
                        opts.ansi_enabled,
                        opts.trans_fields,
                        opts.matching_fields,
                        opts.delimiter,
                    );
                    Arc::new(item)
                }
                SendRawOrBuild::Raw => {
                    let item: Box<str> = line.into();
                    Arc::new(item)
                }
            };

            string_intern.insert_unique_unchecked(key, Arc::downgrade(&item));
            tx_item.send(item)
        }
    }
}

#[inline]
fn hash(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;

    let mut hash = ahash::AHasher::default();

    hash.write(bytes);
    hash.finish()
}
