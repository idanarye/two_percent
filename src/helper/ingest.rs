/// helper for turn a BufRead into a skim stream
use std::io::BufRead;
use std::sync::Arc;

use crossbeam::channel::Sender;
use regex::Regex;

use crate::field::FieldRange;
use crate::SkimItem;

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

    loop {
        // first, read lots of bytes into the buffer
        bytes_buffer = if let Ok(res) = source.fill_buf() {
            res.to_vec()
        } else {
            break;
        };
        source.consume(bytes_buffer.len());

        // now, keep reading to make sure we haven't stopped in the middle of a word.
        // no need to add the bytes to the total buf_len, as these bytes are auto-"consumed()",
        // and bytes_buffer will be extended from slice to accommodate the new bytes
        let _ = source.read_until(line_ending, &mut bytes_buffer);

        // break when there is nothing left to read
        if bytes_buffer.is_empty() {
            break;
        }

        let chunk = std::str::from_utf8(&bytes_buffer).expect("Could not convert bytes to UTF8.");

        let line_iter = chunk.split(['\n', line_ending as char]).map(|line| {
            if line.ends_with("\r\n") {
                line.trim_end_matches("\r\n")
            } else if line.ends_with('\r') {
                line.trim_end_matches('\r')
            } else {
                line
            }
        });

        for line in line_iter {
            if send(line, &opts, &tx_item).is_none() {
                return;
            }
        }
    }
}

fn send(line: &str, opts: &SendRawOrBuild, tx_item: &Sender<Arc<dyn SkimItem>>) -> Option<()> {
    let res = match opts {
        SendRawOrBuild::Build(opts) => {
            let item = DefaultSkimItem::new(
                line.into(),
                opts.ansi_enabled,
                opts.trans_fields,
                opts.matching_fields,
                opts.delimiter,
            );
            tx_item.try_send(Arc::new(item))
        }
        SendRawOrBuild::Raw => {
            let boxed: Box<str> = line.into();
            tx_item.try_send(Arc::new(boxed))
        }
    };

    match res {
        Err(err) if err.is_disconnected() => None,
        _ => Some(()),
    }
}
