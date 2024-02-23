use crate::ansi::ANSIParser;
use crate::field::{parse_matching_fields, parse_transform_fields, FieldRange};
use crate::{AnsiString, DisplayContext, Matches, SkimItem};
use regex::Regex;
use std::borrow::Cow;
use tuikit::prelude::Attr;

//------------------------------------------------------------------------------
/// An item will store everything that one line input will need to be operated and displayed.
///
/// What's special about an item?
/// The simplest version of an item is a line of string, but things are getting more complex:
/// - The conversion of lower/upper case is slow in rust, because it involds unicode.
/// - We may need to interpret the ANSI codes in the text.
/// - The text can be transformed and limited while searching.
///
/// About the ANSI, we made assumption that it is linewise, that means no ANSI codes will affect
/// more than one line.
#[derive(Debug)]
pub struct DefaultSkimItem {
    /// The text that will be output when user press `enter`
    /// `Some(..)` => the original input is transformed, could not output `text` directly
    /// `None` => that it is safe to output `text` directly
    orig_text: Option<Box<str>>,

    /// The text that will be shown on screen and matched.
    text: AnsiString,

    // Option<Box<_>> to reduce memory use in normal cases where no matching ranges are specified.
    matching_ranges: Option<Box<[(usize, usize)]>>,
}

impl DefaultSkimItem {
    pub fn new(
        orig_text: &str,
        ansi_enabled: bool,
        trans_fields: &[FieldRange],
        matching_fields: &[FieldRange],
        delimiter: &Regex,
    ) -> Self {
        let using_transform_fields = !trans_fields.is_empty();

        //        transformed | ANSI             | output
        //------------------------------------------------------
        //                    +- T -> trans+ANSI | ANSI
        //                    |                  |
        //      +- T -> trans +- F -> trans      | orig
        // orig |                                |
        //      +- F -> orig  +- T -> ANSI     ==| ANSI
        //                    |                  |
        //                    +- F -> orig       | orig

        let mut ansi_parser: ANSIParser = Default::default();

        let (orig_text, text) = if using_transform_fields && ansi_enabled {
            // ansi and transform
            let transformed = ansi_parser.parse_ansi(&parse_transform_fields(delimiter, orig_text, trans_fields));
            (Some(orig_text.into()), transformed)
        } else if using_transform_fields {
            // transformed, not ansi
            let transformed = parse_transform_fields(delimiter, orig_text, trans_fields).into();
            (Some(orig_text.into()), transformed)
        } else {
            // normal case
            (None, ansi_parser.parse_ansi(orig_text))
        };

        let matching_ranges = if !matching_fields.is_empty() {
            Some(
                parse_matching_fields(delimiter, text.stripped(), matching_fields)
                    .as_slice()
                    .into(),
            )
        } else {
            None
        };

        DefaultSkimItem {
            orig_text,
            text,
            matching_ranges,
        }
    }
}

impl SkimItem for DefaultSkimItem {
    #[inline]
    fn text(&self) -> Cow<str> {
        Cow::Borrowed(self.text.stripped())
    }

    fn output(&self) -> Cow<str> {
        match &self.orig_text {
            Some(orig_text) if self.text.has_attrs() => {
                let mut ansi_parser: ANSIParser = Default::default();
                let text = ansi_parser.parse_ansi(orig_text);
                Cow::Owned(text.into_inner().to_string())
            }
            Some(orig_text) => Cow::Borrowed(orig_text),
            None => Cow::Borrowed(self.text.stripped()),
        }
    }

    fn get_matching_ranges(&self) -> Option<&[(usize, usize)]> {
        self.matching_ranges.as_ref().map(|vec| vec as &[(usize, usize)])
    }

    fn display(&self, context: DisplayContext) -> AnsiString {
        let new_fragments: Vec<(Attr, (u32, u32))> = match context.matches {
            Some(Matches::CharIndices(indices)) => indices
                .iter()
                .map(|&idx| (context.highlight_attr, (idx as u32, idx as u32 + 1)))
                .collect(),
            Some(Matches::CharRange(start, end)) => vec![(context.highlight_attr, (start as u32, end as u32))],
            Some(Matches::ByteRange(start, end)) => {
                let ch_start = context.text[..start].len();
                let ch_end = ch_start + context.text[start..end].len();
                vec![(context.highlight_attr, (ch_start as u32, ch_end as u32))]
            }
            None => vec![],
        };
        let mut ret = self.text.clone();
        ret.override_attrs(new_fragments);
        ret
    }
}
