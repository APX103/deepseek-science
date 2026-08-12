//! Strict compatibility parser for DeepSeek's textual DSML tool protocol.
//!
//! DSML is a control-plane fallback: a valid block is converted to canonical
//! tool calls by the Runner, while malformed protocol is rejected before any
//! text is published. Markdown code examples remain ordinary assistant text.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value};

/// Keep a broken or hostile provider response from growing the whole-turn
/// quarantine without bound. This is comfortably above the configured model
/// output budgets while still giving the boundary an explicit ceiling.
pub(crate) const MAX_ASSISTANT_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_TOOL_CALLS_PER_TURN: usize = 32;
pub(crate) const MAX_TOOL_CALL_ID_BYTES: usize = 256;
pub(crate) const MAX_TOOL_NAME_BYTES: usize = 256;
pub(crate) const MAX_TOOL_PARAMETERS_PER_CALL: usize = 128;
pub(crate) const MAX_TOOL_PARAMETER_NAME_BYTES: usize = 256;
pub(crate) const MAX_TOOL_ARGUMENT_BYTES_PER_CALL: usize = 512 * 1024;
pub(crate) const MAX_TOOL_ARGUMENT_BYTES_TOTAL: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedDsmlCall {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedAssistantText {
    Plain(String),
    ToolCalls {
        visible_text: String,
        calls: Vec<ParsedDsmlCall>,
    },
}

/// The terminal decision from [`IncrementalAssistantTextGuard`]. Plain text
/// contains only the suffix that has not already crossed the live event
/// boundary; tool calls are the canonical result of the whole-turn parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IncrementalAssistantTextResult {
    Plain(String),
    ToolCalls {
        visible_text: String,
        calls: Vec<ParsedDsmlCall>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncrementalAssistantTextState {
    LeadingWhitespace,
    StreamingPlain,
}

/// Incrementally releases only text that cannot be part of a textual DSML
/// envelope. Every DSML marker starts with a raw ASCII `<`, so retaining the
/// first such byte and everything after it is deliberately conservative and
/// chunk-boundary independent. The complete bounded turn is still retained
/// for exactly one canonical parse in [`Self::finish`].
#[derive(Debug)]
pub(crate) struct IncrementalAssistantTextGuard {
    text: String,
    state: IncrementalAssistantTextState,
    scan_cursor: usize,
    released_cursor: usize,
    publication_frozen: bool,
    observed_raw_lt: bool,
    #[cfg(test)]
    scan_work_bytes: usize,
}

impl Default for IncrementalAssistantTextGuard {
    fn default() -> Self {
        Self {
            text: String::new(),
            state: IncrementalAssistantTextState::LeadingWhitespace,
            scan_cursor: 0,
            released_cursor: 0,
            publication_frozen: false,
            observed_raw_lt: false,
            #[cfg(test)]
            scan_work_bytes: 0,
        }
    }
}

impl IncrementalAssistantTextGuard {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Buffer one provider delta and return the newly irrevocable plain-text
    /// prefix, if any. A failed size check leaves the guard unchanged.
    pub(crate) fn push(&mut self, chunk: &str) -> Result<Option<String>, DsmlError> {
        let next_len = self
            .text
            .len()
            .checked_add(chunk.len())
            .ok_or(DsmlError::TooLarge)?;
        if next_len > MAX_ASSISTANT_TEXT_BYTES {
            return Err(DsmlError::TooLarge);
        }
        self.text.push_str(chunk);

        let publication_was_frozen = self.publication_frozen;
        if self.observed_raw_lt {
            return Ok(None);
        }

        if self.state == IncrementalAssistantTextState::LeadingWhitespace {
            while self.scan_cursor < self.text.len() {
                let character = self.text[self.scan_cursor..]
                    .chars()
                    .next()
                    .expect("scan cursor is in bounds");
                let width = character.len_utf8();
                self.record_scan_work(width);
                self.scan_cursor += width;

                if character.is_whitespace() {
                    continue;
                }
                if character == '<' {
                    self.observed_raw_lt = true;
                    self.freeze_publication();
                    return Ok(None);
                }
                self.state = IncrementalAssistantTextState::StreamingPlain;
                break;
            }
            if self.state == IncrementalAssistantTextState::LeadingWhitespace {
                return Ok(None);
            }
        }

        debug_assert_eq!(self.state, IncrementalAssistantTextState::StreamingPlain);
        let mut safe_end = self.text.len();
        while self.scan_cursor < self.text.len() {
            let byte = self.text.as_bytes()[self.scan_cursor];
            self.record_scan_work(1);
            if byte == b'<' {
                safe_end = self.scan_cursor;
                self.scan_cursor += 1;
                self.observed_raw_lt = true;
                self.freeze_publication();
                break;
            }
            self.scan_cursor += 1;
        }

        if publication_was_frozen || safe_end == self.released_cursor {
            return Ok(None);
        }
        let released = self.text[self.released_cursor..safe_end].to_string();
        self.released_cursor = safe_end;
        Ok(Some(released))
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.text.len()
    }

    /// Stop incremental publication without discarding the canonical buffer.
    /// Runner uses this to couple the reasoning and answer channels once
    /// either one observes a raw `<` control-plane boundary.
    pub(crate) fn freeze_publication(&mut self) {
        self.publication_frozen = true;
    }

    pub(crate) fn publication_is_frozen(&self) -> bool {
        self.publication_frozen
    }

    /// Whether this channel, rather than only its sibling, has observed its
    /// first raw `<`. Scanning continues while externally frozen so Runner can
    /// retain an exact cross-channel event boundary.
    pub(crate) fn has_observed_raw_lt(&self) -> bool {
        self.observed_raw_lt
    }

    /// Byte offset already delivered across the event boundary.
    pub(crate) fn released_cursor(&self) -> usize {
        self.released_cursor
    }

    /// Make the canonical whole-turn decision and return only terminal text
    /// that was not emitted by [`Self::push`].
    pub(crate) fn finish(self) -> Result<IncrementalAssistantTextResult, DsmlError> {
        match parse_assistant_text(&self.text)? {
            ParsedAssistantText::Plain(text) => {
                debug_assert_eq!(text, self.text);
                Ok(IncrementalAssistantTextResult::Plain(
                    text[self.released_cursor..].to_string(),
                ))
            }
            ParsedAssistantText::ToolCalls {
                visible_text,
                calls,
            } => Ok(IncrementalAssistantTextResult::ToolCalls {
                visible_text,
                calls,
            }),
        }
    }

    fn record_scan_work(&mut self, bytes: usize) {
        #[cfg(test)]
        {
            self.scan_work_bytes += bytes;
        }
        #[cfg(not(test))]
        let _ = bytes;
    }

    #[cfg(test)]
    fn scan_work_bytes(&self) -> usize {
        self.scan_work_bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DsmlError {
    #[error("assistant text exceeds the DSML quarantine limit")]
    TooLarge,
    #[error("malformed DSML tag")]
    MalformedTag,
    #[error("unknown DSML control tag")]
    UnknownTag,
    #[error("DSML tag has invalid attributes")]
    InvalidAttributes,
    #[error("DSML control tags are out of order")]
    UnexpectedTag,
    #[error("DSML control block is not closed")]
    UnclosedBlock,
    #[error("DSML tool-call batch is empty")]
    EmptyBatch,
    #[error("DSML parameter name is duplicated")]
    DuplicateParameter,
    #[error("DSML parameter has an invalid string flag")]
    InvalidStringFlag,
    #[error("DSML non-string parameter is not valid JSON")]
    InvalidJson,
    #[error("DSML tool-call batch exceeds its configured boundary")]
    BoundaryExceeded,
    #[error("multiple DSML tool-call blocks are not allowed in one turn")]
    MultipleBlocks,
    #[error("DSML control protocol is not in an executable top-level block")]
    InvalidContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagName {
    ToolCalls,
    Invoke,
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DsmlTag {
    name: TagName,
    closing: bool,
    attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocatedTag {
    start: usize,
    end: usize,
    tag: DsmlTag,
}

enum CandidateTag {
    NotDsml,
    Dsml(DsmlTag),
}

/// Parse one complete assistant text turn. This function is intentionally
/// side-effect free; accepting a call never implies authorization or
/// execution. The compatibility protocol is a stand-alone envelope: outside
/// its one complete tool-call block, only whitespace is allowed. That simple
/// invariant prevents Markdown containers invisible to the UI from becoming
/// an execution context.
pub(crate) fn parse_assistant_text(text: &str) -> Result<ParsedAssistantText, DsmlError> {
    if text.len() > MAX_ASSISTANT_TEXT_BYTES {
        return Err(DsmlError::TooLarge);
    }

    let Some(open) = find_next_visible_dsml_tag(text, 0)? else {
        return Ok(ParsedAssistantText::Plain(text.to_string()));
    };
    if open.tag.closing || open.tag.name != TagName::ToolCalls {
        return Err(DsmlError::UnexpectedTag);
    }
    if !is_executable_top_level_opener(text, open.start) {
        return Err(DsmlError::InvalidContext);
    }
    require_attrs(&open.tag, &[])?;

    let (calls, block_end) = parse_tool_calls_block(text, open.end)?;
    if find_next_visible_dsml_tag(text, block_end)?.is_some() {
        return Err(DsmlError::MultipleBlocks);
    }
    if !text[block_end..].chars().all(char::is_whitespace) {
        return Err(DsmlError::InvalidContext);
    }

    Ok(ParsedAssistantText::ToolCalls {
        visible_text: String::new(),
        calls,
    })
}

fn is_executable_top_level_opener(text: &str, start: usize) -> bool {
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &text[line_start..start];
    if prefix.len() > 3 || !prefix.bytes().all(|byte| byte == b' ') {
        return false;
    }

    let before = &text[..start];
    let last_comment_open = before.rfind("<!--");
    let last_comment_close = before.rfind("-->");
    text[..line_start].chars().all(char::is_whitespace)
        && last_comment_open.is_none_or(|open| last_comment_close.is_some_and(|close| close > open))
        && !is_inside_commonmark_raw_html_block(text, start)
        && !has_abandoned_container_fence_before(text, start)
}

#[derive(Debug, Clone, Copy)]
enum RawHtmlBlockEnd {
    BlankLine,
    Literal(&'static str),
    AsciiCaseInsensitive(&'static str),
}

impl RawHtmlBlockEnd {
    fn occurs_on(self, line: &str) -> bool {
        match self {
            Self::BlankLine => line.trim().is_empty(),
            Self::Literal(terminator) => line.contains(terminator),
            Self::AsciiCaseInsensitive(terminator) => {
                line.to_ascii_lowercase().contains(terminator)
            }
        }
    }
}

/// CommonMark raw HTML blocks are inert in the UI (`skipHtml`). This secondary
/// scanner covers line-start forms of all seven classes; the stand-alone
/// envelope above also rejects inline forms because their prefix is non-empty.
fn is_inside_commonmark_raw_html_block(text: &str, target: usize) -> bool {
    let target_line = text[..target].rfind('\n').map_or(0, |index| index + 1);
    let mut active: Option<RawHtmlBlockEnd> = None;
    let mut cursor = 0usize;

    while cursor < target_line {
        let end = line_end(text, cursor).min(target_line.saturating_sub(1));
        let line = &text[cursor..end];
        if let Some(terminator) = active {
            if terminator.occurs_on(line) {
                active = None;
            }
        } else if let Some(terminator) = raw_html_block_opener(line) {
            if !terminator.occurs_on(line) {
                active = Some(terminator);
            }
        }
        cursor = if end < target_line {
            end + 1
        } else {
            target_line
        };
    }
    active.is_some()
}

fn raw_html_block_opener(line: &str) -> Option<RawHtmlBlockEnd> {
    let (indent, content_start) = markdown_indent(line, 0);
    if indent > 3 {
        return None;
    }
    let line = &line[content_start..];
    if line.starts_with("<!--") {
        return Some(RawHtmlBlockEnd::Literal("-->"));
    }
    if line.starts_with("<?") {
        return Some(RawHtmlBlockEnd::Literal("?>"));
    }
    if line.starts_with("<![CDATA[") {
        return Some(RawHtmlBlockEnd::Literal("]]>"));
    }
    if line.as_bytes().get(2).is_some_and(u8::is_ascii_uppercase) && line.starts_with("<!") {
        return Some(RawHtmlBlockEnd::Literal(">"));
    }

    let (closing, name, after_name) = html_tag_head(line)?;
    if !closing {
        for raw_text_tag in ["script", "pre", "style", "textarea"] {
            if name.eq_ignore_ascii_case(raw_text_tag) {
                return Some(RawHtmlBlockEnd::AsciiCaseInsensitive(match raw_text_tag {
                    "script" => "</script>",
                    "pre" => "</pre>",
                    "style" => "</style>",
                    "textarea" => "</textarea>",
                    _ => unreachable!(),
                }));
            }
        }
    }

    if is_commonmark_block_tag(name) || is_complete_html_tag_tail(after_name) {
        return Some(RawHtmlBlockEnd::BlankLine);
    }
    None
}

fn html_tag_head(line: &str) -> Option<(bool, &str, &str)> {
    let mut cursor = 1usize;
    if line.as_bytes().first() != Some(&b'<') {
        return None;
    }
    let closing = line.as_bytes().get(cursor) == Some(&b'/');
    if closing {
        cursor += 1;
    }
    let name_start = cursor;
    if !line
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_alphabetic)
    {
        return None;
    }
    cursor += 1;
    while line
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    {
        cursor += 1;
    }
    let boundary = line.as_bytes().get(cursor);
    if boundary.is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'/' | b'>')) {
        return None;
    }
    Some((closing, &line[name_start..cursor], &line[cursor..]))
}

fn is_complete_html_tag_tail(tail: &str) -> bool {
    let mut quote = None;
    for (offset, character) in tail.char_indices() {
        match quote {
            Some(expected) if character == expected => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == '>' => {
                return tail[offset + 1..]
                    .chars()
                    .all(|rest| matches!(rest, ' ' | '\t' | '\r'));
            }
            None => {}
        }
    }
    false
}

fn is_commonmark_block_tag(name: &str) -> bool {
    const BLOCK_TAGS: &[&str] = &[
        "address",
        "article",
        "aside",
        "base",
        "basefont",
        "blockquote",
        "body",
        "caption",
        "center",
        "col",
        "colgroup",
        "dd",
        "details",
        "dialog",
        "dir",
        "div",
        "dl",
        "dt",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "frame",
        "frameset",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hr",
        "html",
        "iframe",
        "legend",
        "li",
        "link",
        "main",
        "menu",
        "menuitem",
        "nav",
        "noframes",
        "ol",
        "optgroup",
        "option",
        "p",
        "param",
        "search",
        "section",
        "summary",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "title",
        "tr",
        "track",
        "ul",
    ];
    BLOCK_TAGS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn has_abandoned_container_fence_before(text: &str, target: usize) -> bool {
    let protected = complete_markdown_block_ranges(text);
    let mut protected_index = 0usize;
    let mut active: Option<(char, usize, FenceContainer)> = None;
    let mut cursor = 0usize;

    while cursor <= target && cursor < text.len() {
        while protected_index < protected.len() && protected[protected_index].1 <= cursor {
            protected_index += 1;
        }
        if let Some((start, end)) = protected.get(protected_index).copied() {
            if start <= cursor {
                cursor = end;
                protected_index += 1;
                continue;
            }
        }

        let end = line_end(text, cursor);
        let next = if end < text.len() { end + 1 } else { end };
        let (blockquote_depth, indent_columns, content_start) =
            markdown_blockquote_prefix(text, cursor, end);
        let logical_blank = text[content_start..end].trim().is_empty();

        if let Some((marker, width, container)) = active {
            let container_continues = blockquote_depth == container.blockquote_depth
                && container
                    .list_indent
                    .is_none_or(|list_indent| logical_blank || indent_columns >= list_indent);
            if !container_continues {
                return true;
            }
            let indent_matches = match container.list_indent {
                Some(list_indent) => {
                    (list_indent..=list_indent.saturating_add(3)).contains(&indent_columns)
                }
                None => indent_columns <= 3,
            };
            let candidate_width = text[content_start..end]
                .chars()
                .take_while(|candidate| *candidate == marker)
                .count();
            let after_fence = content_start + marker.len_utf8() * candidate_width;
            let valid_close = indent_matches
                && candidate_width >= width
                && text[after_fence..end]
                    .chars()
                    .all(|candidate| matches!(candidate, ' ' | '\t' | '\r'));
            if valid_close {
                active = None;
            }
        } else if let Some((_, marker, width, container)) = fence_opening_at_line(text, cursor, end)
        {
            if container.blockquote_depth > 0 || container.list_indent.is_some() {
                active = Some((marker, width, container));
            }
        }
        cursor = next;
    }
    false
}

fn parse_tool_calls_block(
    text: &str,
    mut cursor: usize,
) -> Result<(Vec<ParsedDsmlCall>, usize), DsmlError> {
    let mut calls = Vec::new();
    let mut total_argument_bytes = 0usize;

    loop {
        let next = find_next_raw_dsml_tag(text, cursor)?.ok_or(DsmlError::UnclosedBlock)?;
        require_whitespace(&text[cursor..next.start])?;

        match (next.tag.closing, next.tag.name) {
            (true, TagName::ToolCalls) => {
                require_attrs(&next.tag, &[])?;
                if calls.is_empty() {
                    return Err(DsmlError::EmptyBatch);
                }
                return Ok((calls, next.end));
            }
            (false, TagName::Invoke) => {
                let name = required_single_attr(&next.tag, "name")?;
                if name.is_empty() {
                    return Err(DsmlError::InvalidAttributes);
                }
                if name.len() > MAX_TOOL_NAME_BYTES {
                    return Err(DsmlError::BoundaryExceeded);
                }
                let (call, end) = parse_invoke(text, next.end, name)?;
                if calls.len() >= MAX_TOOL_CALLS_PER_TURN {
                    return Err(DsmlError::BoundaryExceeded);
                }
                total_argument_bytes = total_argument_bytes
                    .checked_add(call.arguments.len())
                    .filter(|total| *total <= MAX_TOOL_ARGUMENT_BYTES_TOTAL)
                    .ok_or(DsmlError::BoundaryExceeded)?;
                calls.push(call);
                cursor = end;
            }
            _ => return Err(DsmlError::UnexpectedTag),
        }
    }
}

fn parse_invoke(
    text: &str,
    mut cursor: usize,
    name: String,
) -> Result<(ParsedDsmlCall, usize), DsmlError> {
    let mut arguments = Map::new();
    let mut argument_body_bytes = 0usize;

    loop {
        let next = find_next_raw_dsml_tag(text, cursor)?.ok_or(DsmlError::UnclosedBlock)?;
        require_whitespace(&text[cursor..next.start])?;

        match (next.tag.closing, next.tag.name) {
            (true, TagName::Invoke) => {
                require_attrs(&next.tag, &[])?;
                let arguments = Value::Object(arguments).to_string();
                if arguments.len() > MAX_TOOL_ARGUMENT_BYTES_PER_CALL {
                    return Err(DsmlError::BoundaryExceeded);
                }
                return Ok((ParsedDsmlCall { name, arguments }, next.end));
            }
            (false, TagName::Parameter) => {
                let parameter_name = required_attr(&next.tag, "name")?;
                let string_flag = required_attr(&next.tag, "string")?;
                if next.tag.attrs.len() != 2 || parameter_name.is_empty() {
                    return Err(DsmlError::InvalidAttributes);
                }
                if parameter_name.len() > MAX_TOOL_PARAMETER_NAME_BYTES {
                    return Err(DsmlError::BoundaryExceeded);
                }
                if arguments.len() >= MAX_TOOL_PARAMETERS_PER_CALL {
                    return Err(DsmlError::BoundaryExceeded);
                }
                if arguments.contains_key(&parameter_name) {
                    return Err(DsmlError::DuplicateParameter);
                }
                let is_string = match string_flag.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(DsmlError::InvalidStringFlag),
                };
                let (value, body_bytes, end) = parse_parameter(text, next.end, is_string)?;
                argument_body_bytes = argument_body_bytes
                    .checked_add(body_bytes)
                    .filter(|total| *total <= MAX_TOOL_ARGUMENT_BYTES_PER_CALL)
                    .ok_or(DsmlError::BoundaryExceeded)?;
                arguments.insert(parameter_name, value);
                cursor = end;
            }
            _ => return Err(DsmlError::UnexpectedTag),
        }
    }
}

fn parse_parameter(
    text: &str,
    cursor: usize,
    is_string: bool,
) -> Result<(Value, usize, usize), DsmlError> {
    let close = find_next_raw_dsml_tag(text, cursor)?.ok_or(DsmlError::UnclosedBlock)?;
    if !close.tag.closing || close.tag.name != TagName::Parameter {
        return Err(DsmlError::UnexpectedTag);
    }
    require_attrs(&close.tag, &[])?;

    let body = &text[cursor..close.start];
    if body.len() > MAX_TOOL_ARGUMENT_BYTES_PER_CALL {
        return Err(DsmlError::BoundaryExceeded);
    }
    let value = if is_string {
        Value::String(body.to_string())
    } else {
        serde_json::from_str(body.trim()).map_err(|_| DsmlError::InvalidJson)?
    };
    Ok((value, body.len(), close.end))
}

fn require_whitespace(text: &str) -> Result<(), DsmlError> {
    if text.chars().all(char::is_whitespace) {
        Ok(())
    } else {
        Err(DsmlError::UnexpectedTag)
    }
}

fn required_single_attr(tag: &DsmlTag, key: &str) -> Result<String, DsmlError> {
    if tag.attrs.len() != 1 {
        return Err(DsmlError::InvalidAttributes);
    }
    required_attr(tag, key)
}

fn required_attr(tag: &DsmlTag, key: &str) -> Result<String, DsmlError> {
    tag.attrs
        .get(key)
        .cloned()
        .ok_or(DsmlError::InvalidAttributes)
}

fn require_attrs(tag: &DsmlTag, expected: &[&str]) -> Result<(), DsmlError> {
    if tag.attrs.len() == expected.len()
        && expected.iter().all(|name| tag.attrs.contains_key(*name))
    {
        Ok(())
    } else {
        Err(DsmlError::InvalidAttributes)
    }
}

/// Locate control protocol while honoring only *complete* Markdown code
/// regions. An unmatched backtick/fence remains ordinary text and therefore
/// cannot conceal a DSML marker.
fn find_next_visible_dsml_tag(
    text: &str,
    mut cursor: usize,
) -> Result<Option<LocatedTag>, DsmlError> {
    let protected_blocks = complete_markdown_block_ranges(text);
    let backticks = BacktickIndex::build(text, &protected_blocks);
    let mut protected_index = protected_blocks.partition_point(|(_, end)| *end <= cursor);

    while cursor < text.len() {
        while protected_index < protected_blocks.len()
            && protected_blocks[protected_index].1 <= cursor
        {
            protected_index += 1;
        }
        if let Some((start, end)) = protected_blocks.get(protected_index).copied() {
            if start <= cursor {
                cursor = end;
                protected_index += 1;
                continue;
            }
        }

        if is_line_start(text, cursor) {
            let end = line_end(text, cursor);
            if let Some((content_start, marker, width, _)) =
                fence_opening_at_line(text, cursor, end)
            {
                // Complete fences are already represented by a protected
                // range. A remaining run is an unterminated/invalid fence and
                // must stay literal rather than becoming a multiline inline
                // span that conceals protocol.
                cursor = content_start + marker.len_utf8() * width;
                continue;
            }
        }

        match text.as_bytes()[cursor] {
            b'`' => {
                let raw_width = text.as_bytes()[cursor..]
                    .iter()
                    .take_while(|byte| **byte == b'`')
                    .count();
                let raw_end = cursor + raw_width;
                cursor = normalized_backtick_run(text, cursor, raw_width)
                    .and_then(|(start, width)| backticks.closing_end(start, width))
                    .unwrap_or(raw_end);
            }
            b'<' => match parse_candidate_tag(text, cursor)? {
                CandidateTag::Dsml(tag) => {
                    let end = find_tag_end(text, cursor)?;
                    return Ok(Some(LocatedTag {
                        start: cursor,
                        end,
                        tag,
                    }));
                }
                CandidateTag::NotDsml => cursor += 1,
            },
            _ => cursor += next_char_len(text, cursor),
        }
    }
    Ok(None)
}

/// Locate protocol inside an already-established DSML block. Markdown has no
/// meaning here because parameter bodies are control payload, not UI prose.
fn find_next_raw_dsml_tag(text: &str, mut cursor: usize) -> Result<Option<LocatedTag>, DsmlError> {
    while cursor < text.len() {
        let Some(relative) = text[cursor..].find('<') else {
            return Ok(None);
        };
        let start = cursor + relative;
        match parse_candidate_tag(text, start)? {
            CandidateTag::Dsml(tag) => {
                return Ok(Some(LocatedTag {
                    start,
                    end: find_tag_end(text, start)?,
                    tag,
                }));
            }
            CandidateTag::NotDsml => cursor = start + 1,
        }
    }
    Ok(None)
}

fn parse_candidate_tag(text: &str, start: usize) -> Result<CandidateTag, DsmlError> {
    debug_assert_eq!(text.as_bytes().get(start), Some(&b'<'));
    if !has_dsml_candidate_prefix(&text[start + 1..]) {
        return Ok(CandidateTag::NotDsml);
    }
    let Some(relative_end) = text[start + 1..].find('>') else {
        let tail = text[start + 1..].trim();
        let tail = tail.strip_prefix('/').unwrap_or(tail).trim_start();
        let head_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        return match parse_dsml_head(&tail[..head_end]) {
            Ok(None) => Ok(CandidateTag::NotDsml),
            Ok(Some(_)) | Err(_) => Err(DsmlError::MalformedTag),
        };
    };
    let inside = &text[start + 1..start + 1 + relative_end];
    let inside = inside.trim();
    let (closing, inside) = match inside.strip_prefix('/') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, inside),
    };
    let head_end = inside.find(char::is_whitespace).unwrap_or(inside.len());
    let head = &inside[..head_end];
    let Some(name) = parse_dsml_head(head)? else {
        return Ok(CandidateTag::NotDsml);
    };
    let attrs = parse_attributes(&inside[head_end..])?;
    if closing && !attrs.is_empty() {
        return Err(DsmlError::InvalidAttributes);
    }
    Ok(CandidateTag::Dsml(DsmlTag {
        name,
        closing,
        attrs,
    }))
}

/// Constant-lookahead guard used before searching for a closing `>`. Without
/// this, an ordinary document containing many `<x` fragments makes every
/// candidate rescan the entire remaining suffix.
fn has_dsml_candidate_prefix(mut text: &str) -> bool {
    if let Some(rest) = text.strip_prefix('/') {
        text = rest;
    }
    let (bars, rest, _, _) = take_protocol_bars(text);
    if bars == 0 {
        return false;
    }
    if bars > 2 {
        return true;
    }
    let probe: String = rest
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .take(4)
        .collect::<String>()
        .to_ascii_uppercase();
    "DSML".starts_with(&probe) || probe == "DSML"
}

fn parse_dsml_head(head: &str) -> Result<Option<TagName>, DsmlError> {
    let (leading_bars, after_leading, leading_marker, leading_consistent) =
        take_protocol_bars(head);
    if leading_bars == 0 {
        return Ok(None);
    }
    if leading_bars > 2 || !leading_consistent {
        return Err(DsmlError::MalformedTag);
    }

    let Some(dsml) = after_leading.get(..4) else {
        return if "DSML".starts_with(&after_leading.to_ascii_uppercase()) {
            Err(DsmlError::MalformedTag)
        } else {
            Ok(None)
        };
    };
    if !dsml.eq_ignore_ascii_case("DSML") {
        return Ok(None);
    }
    let (trailing_bars, tag_name, trailing_marker, trailing_consistent) =
        take_protocol_bars(&after_leading[4..]);
    if !(1..=2).contains(&trailing_bars)
        || !trailing_consistent
        || leading_bars != trailing_bars
        || leading_marker != trailing_marker
    {
        return Err(DsmlError::MalformedTag);
    }

    match tag_name.to_ascii_lowercase().as_str() {
        "tool_calls" => Ok(Some(TagName::ToolCalls)),
        "invoke" => Ok(Some(TagName::Invoke)),
        "parameter" => Ok(Some(TagName::Parameter)),
        _ => Err(DsmlError::UnknownTag),
    }
}

fn take_protocol_bars(text: &str) -> (usize, &str, Option<char>, bool) {
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut marker = None;
    let mut consistent = true;
    for candidate in text.chars() {
        if !matches!(candidate, '|' | '｜') {
            break;
        }
        if marker.is_some_and(|expected| expected != candidate) {
            consistent = false;
        }
        marker.get_or_insert(candidate);
        count += 1;
        bytes += candidate.len_utf8();
    }
    (count, &text[bytes..], marker, consistent)
}

fn parse_attributes(mut input: &str) -> Result<BTreeMap<String, String>, DsmlError> {
    let mut attrs = BTreeMap::new();
    loop {
        input = input.trim_start();
        if input.is_empty() {
            return Ok(attrs);
        }

        let key_end = input
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
            .unwrap_or(input.len());
        if key_end == 0 {
            return Err(DsmlError::MalformedTag);
        }
        let key = input[..key_end].to_ascii_lowercase();
        input = input[key_end..].trim_start();
        let Some(rest) = input.strip_prefix('=') else {
            return Err(DsmlError::MalformedTag);
        };
        input = rest.trim_start();
        if input.is_empty() {
            return Err(DsmlError::MalformedTag);
        }

        let (value, rest) = match input.chars().next().expect("non-empty") {
            quote @ ('\'' | '"') => {
                let body = &input[quote.len_utf8()..];
                let Some(close) = body.find(quote) else {
                    return Err(DsmlError::MalformedTag);
                };
                (body[..close].to_string(), &body[close + quote.len_utf8()..])
            }
            _ => {
                let end = input.find(char::is_whitespace).unwrap_or(input.len());
                (input[..end].to_string(), &input[end..])
            }
        };
        if attrs.insert(key, value).is_some() {
            return Err(DsmlError::InvalidAttributes);
        }
        input = rest;
    }
}

fn find_tag_end(text: &str, start: usize) -> Result<usize, DsmlError> {
    text[start + 1..]
        .find('>')
        .map(|offset| start + 1 + offset + 1)
        .ok_or(DsmlError::MalformedTag)
}

fn is_line_start(text: &str, index: usize) -> bool {
    index == 0 || text.as_bytes().get(index.wrapping_sub(1)) == Some(&b'\n')
}

fn markdown_indent(text: &str, mut cursor: usize) -> (usize, usize) {
    let mut columns = 0usize;
    while let Some(byte) = text.as_bytes().get(cursor) {
        match byte {
            b' ' => {
                columns += 1;
                cursor += 1;
            }
            b'\t' => {
                columns += 4 - (columns % 4);
                cursor += 1;
            }
            _ => break,
        }
    }
    (columns, cursor)
}

fn fence_run_at(text: &str, start: usize) -> Option<(char, usize)> {
    let marker = text[start..].chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let width = text[start..]
        .chars()
        .take_while(|candidate| *candidate == marker)
        .count();
    (width >= 3).then_some((marker, width))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FenceContainer {
    blockquote_depth: usize,
    list_indent: Option<usize>,
}

fn fence_opening_at_line(
    text: &str,
    line_start: usize,
    line_end: usize,
) -> Option<(usize, char, usize, FenceContainer)> {
    let (blockquote_depth, indent_columns, mut content_start) =
        markdown_blockquote_prefix(text, line_start, line_end);
    if indent_columns > 3 {
        return None;
    }

    let mut list_indent = None;
    if let Some((after_marker, marker_columns)) = list_marker(text, content_start, line_end) {
        let (following_indent, after_indent) = markdown_indent(text, after_marker);
        if following_indent == 0 || after_indent > line_end {
            return None;
        }
        list_indent = Some(indent_columns + marker_columns + following_indent);
        content_start = after_indent;
    }

    let (marker, width) = fence_run_at(text, content_start)?;
    let after_open = content_start + marker.len_utf8() * width;
    let valid_open = marker != '`' || !text[after_open..line_end].contains('`');
    valid_open.then_some((
        content_start,
        marker,
        width,
        FenceContainer {
            blockquote_depth,
            list_indent,
        },
    ))
}

fn markdown_blockquote_prefix(
    text: &str,
    line_start: usize,
    line_end: usize,
) -> (usize, usize, usize) {
    let mut cursor = line_start;
    let mut depth = 0usize;
    loop {
        let (indent, content) = markdown_indent(text, cursor);
        if indent > 3 || content >= line_end || text.as_bytes().get(content) != Some(&b'>') {
            return (depth, indent, content.min(line_end));
        }
        depth += 1;
        cursor = content + 1;
        if matches!(text.as_bytes().get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }
    }
}

fn list_marker(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let first = *text.as_bytes().get(start)?;
    if matches!(first, b'-' | b'+' | b'*') {
        let after = start + 1;
        return matches!(text.as_bytes().get(after), Some(b' ' | b'\t')).then_some((after, 1));
    }

    let mut cursor = start;
    let mut digits = 0usize;
    while cursor < end && text.as_bytes()[cursor].is_ascii_digit() && digits < 9 {
        cursor += 1;
        digits += 1;
    }
    if digits == 0 || cursor >= end || !matches!(text.as_bytes()[cursor], b'.' | b')') {
        return None;
    }
    let marker_end = cursor + 1;
    matches!(text.as_bytes().get(marker_end), Some(b' ' | b'\t'))
        .then_some((marker_end, digits + 1))
}

/// Precompute complete fenced blocks and four-column indented code in one
/// forward pass. An unterminated fence intentionally contributes no range, so
/// it cannot hide a control marker. The scanner therefore remains linear even
/// for thousands of unmatched fence-like lines.
fn complete_markdown_block_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut active_fence: Option<(usize, char, usize, FenceContainer)> = None;
    let mut cursor = 0usize;
    let mut can_start_indented_code = true;
    let mut in_indented_code = false;

    while cursor < text.len() {
        let end = line_end(text, cursor);
        let next = if end < text.len() { end + 1 } else { end };
        let (blockquote_depth, indent_columns, content_start) =
            markdown_blockquote_prefix(text, cursor, end);
        let logical_blank = text[content_start..end].trim().is_empty();

        if let Some((opening, marker, width, container)) = active_fence {
            let container_continues = blockquote_depth == container.blockquote_depth
                && container
                    .list_indent
                    .is_none_or(|list_indent| logical_blank || indent_columns >= list_indent);
            if !container_continues {
                // A quote/list fence cannot consume later top-level output.
                // Abandoning it leaves the earlier would-be fence unprotected,
                // which is deliberately fail-closed if it contained DSML.
                active_fence = None;
            } else {
                let indent_matches = match container.list_indent {
                    Some(list_indent) => {
                        (list_indent..=list_indent.saturating_add(3)).contains(&indent_columns)
                    }
                    None => indent_columns <= 3,
                };
                let candidate_width = text[content_start..end]
                    .chars()
                    .take_while(|candidate| *candidate == marker)
                    .count();
                let after_fence = content_start + marker.len_utf8() * candidate_width;
                let valid_close = indent_matches
                    && candidate_width >= width
                    && text[after_fence..end]
                        .chars()
                        .all(|candidate| matches!(candidate, ' ' | '\t' | '\r'));
                if valid_close {
                    ranges.push((opening, next));
                    active_fence = None;
                    can_start_indented_code = true;
                    in_indented_code = false;
                }
                cursor = next;
                continue;
            }
        }

        if let Some((_, marker, width, container)) = fence_opening_at_line(text, cursor, end) {
            active_fence = Some((cursor, marker, width, container));
            can_start_indented_code = false;
            in_indented_code = false;
        } else if indent_columns >= 4 && (can_start_indented_code || in_indented_code) {
            ranges.push((cursor, next));
            can_start_indented_code = false;
            in_indented_code = true;
        } else if logical_blank {
            if in_indented_code {
                ranges.push((cursor, next));
            }
            can_start_indented_code = true;
        } else {
            can_start_indented_code = false;
            in_indented_code = false;
        }
        cursor = next;
    }
    ranges
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map(|offset| start + offset)
        .unwrap_or(text.len())
}

#[derive(Debug, Default)]
struct BacktickIndex {
    by_width: HashMap<usize, Vec<(usize, usize)>>,
    eligible_openings: HashSet<usize>,
}

impl BacktickIndex {
    fn build(text: &str, protected_blocks: &[(usize, usize)]) -> Self {
        let mut fence_starts = HashSet::new();
        let mut line_start = 0usize;
        while line_start < text.len() {
            let end = line_end(text, line_start);
            if let Some((content_start, marker, _, _)) =
                fence_opening_at_line(text, line_start, end)
            {
                if marker == '`' {
                    fence_starts.insert(content_start);
                }
            }
            line_start = if end < text.len() { end + 1 } else { end };
        }

        let mut index = Self::default();
        let mut protected_index = 0usize;
        let mut cursor = 0usize;
        while cursor < text.len() {
            while protected_index < protected_blocks.len()
                && protected_blocks[protected_index].1 <= cursor
            {
                protected_index += 1;
            }
            if let Some((start, end)) = protected_blocks.get(protected_index).copied() {
                if start <= cursor {
                    cursor = end;
                    protected_index += 1;
                    continue;
                }
            }

            if text.as_bytes()[cursor] == b'`' {
                let raw_width = text.as_bytes()[cursor..]
                    .iter()
                    .take_while(|byte| **byte == b'`')
                    .count();
                let end = cursor + raw_width;
                if !fence_starts.contains(&cursor) {
                    if is_backslash_escaped(text, cursor) {
                        // Once a code span is open, backslash escaping has no
                        // meaning inside it, so the complete run can still be
                        // its closing delimiter. Outside a span the first tick
                        // is escaped and only the remainder may open one.
                        index
                            .by_width
                            .entry(raw_width)
                            .or_default()
                            .push((cursor, end));
                        if let Some((start, width)) =
                            normalized_backtick_run(text, cursor, raw_width)
                        {
                            index.by_width.entry(width).or_default().push((start, end));
                            index.eligible_openings.insert(start);
                        }
                    } else {
                        index
                            .by_width
                            .entry(raw_width)
                            .or_default()
                            .push((cursor, end));
                        index.eligible_openings.insert(cursor);
                    }
                }
                cursor = end;
            } else {
                cursor += next_char_len(text, cursor);
            }
        }
        index
    }

    fn closing_end(&self, opening: usize, width: usize) -> Option<usize> {
        if !self.eligible_openings.contains(&opening) {
            return None;
        }
        let runs = self.by_width.get(&width)?;
        let position = runs
            .binary_search_by_key(&opening, |(start, _)| *start)
            .ok()?;
        runs.get(position + 1).map(|(_, end)| *end)
    }
}

/// Backslash escaping consumes the first punctuation character, not the whole
/// adjacent run. Thus `\``` leaves a valid two-backtick delimiter. A fully
/// escaped one-backtick run cannot open a span, though it can still close an
/// already-open span because code-span contents do not process backslashes.
fn normalized_backtick_run(text: &str, start: usize, raw_width: usize) -> Option<(usize, usize)> {
    if is_backslash_escaped(text, start) {
        (raw_width > 1).then_some((start + 1, raw_width - 1))
    } else {
        Some((start, raw_width))
    }
}

fn is_backslash_escaped(text: &str, start: usize) -> bool {
    let mut cursor = start;
    let mut backslashes = 0usize;
    while cursor > 0 && text.as_bytes()[cursor - 1] == b'\\' {
        cursor -= 1;
        backslashes += 1;
    }
    backslashes % 2 == 1
}

fn next_char_len(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .chars()
        .next()
        .expect("cursor is in bounds")
        .len_utf8()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(text: &str) -> ParsedDsmlCall {
        match parse_assistant_text(text).expect("valid DSML") {
            ParsedAssistantText::ToolCalls { mut calls, .. } => {
                assert_eq!(calls.len(), 1);
                calls.remove(0)
            }
            ParsedAssistantText::Plain(_) => panic!("expected tool calls"),
        }
    }

    fn run_incremental_guard<'a>(
        source: &str,
        chunks: impl IntoIterator<Item = &'a str>,
    ) -> (
        Vec<String>,
        Result<IncrementalAssistantTextResult, DsmlError>,
    ) {
        let mut guard = IncrementalAssistantTextGuard::new();
        let mut released = Vec::new();
        for chunk in chunks {
            if let Some(text) = guard.push(chunk).expect("chunk fits guard") {
                assert!(!text.is_empty());
                released.push(text);
            }
        }
        assert_eq!(guard.buffered_len(), source.len());
        (released, guard.finish())
    }

    fn assert_incremental_guard_matches_parser<'a>(
        source: &str,
        chunks: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        let canonical = parse_assistant_text(source);
        let (released, terminal) = run_incremental_guard(source, chunks);
        match (canonical, terminal) {
            (
                Ok(ParsedAssistantText::Plain(expected)),
                Ok(IncrementalAssistantTextResult::Plain(remainder)),
            ) => {
                let mut reconstructed = released.concat();
                reconstructed.push_str(&remainder);
                assert_eq!(reconstructed, expected);
            }
            (
                Ok(ParsedAssistantText::ToolCalls {
                    visible_text: expected_visible,
                    calls: expected_calls,
                }),
                Ok(IncrementalAssistantTextResult::ToolCalls {
                    visible_text,
                    calls,
                }),
            ) => {
                assert!(released.is_empty(), "control envelope text was released");
                assert_eq!(visible_text, expected_visible);
                assert_eq!(calls, expected_calls);
            }
            (Err(expected), Err(actual)) => assert_eq!(actual, expected),
            (canonical, terminal) => {
                panic!("guard result {terminal:?} disagreed with canonical parse {canonical:?}")
            }
        }
        released
    }

    fn char_chunks(source: &str) -> Vec<&str> {
        let mut boundaries = source.char_indices().map(|(index, _)| index).skip(1);
        let mut start = 0usize;
        let mut chunks = Vec::new();
        for end in boundaries.by_ref().chain(std::iter::once(source.len())) {
            chunks.push(&source[start..end]);
            start = end;
        }
        if source.is_empty() {
            chunks.push("");
        }
        chunks
    }

    #[test]
    fn incremental_guard_releases_ordinary_chunks_before_finish() {
        let source = " \n\t研究正在进行 🚀";
        let mut guard = IncrementalAssistantTextGuard::new();
        assert_eq!(guard.push(" \n"), Ok(None));
        assert_eq!(guard.push("\t"), Ok(None));
        assert_eq!(guard.push("研究"), Ok(Some(" \n\t研究".into())));
        assert_eq!(guard.push("正在"), Ok(Some("正在".into())));
        assert_eq!(guard.push("进行 🚀"), Ok(Some("进行 🚀".into())));
        assert_eq!(guard.buffered_len(), source.len());
        assert_eq!(
            guard.finish(),
            Ok(IncrementalAssistantTextResult::Plain(String::new()))
        );
    }

    #[test]
    fn incremental_guard_external_freeze_retains_plain_remainder() {
        let mut guard = IncrementalAssistantTextGuard::new();
        assert!(!guard.publication_is_frozen());
        assert_eq!(guard.push("safe prefix"), Ok(Some("safe prefix".into())));
        assert_eq!(guard.released_cursor(), "safe prefix".len());

        guard.freeze_publication();
        assert!(guard.publication_is_frozen());
        assert_eq!(guard.push(" and private suffix"), Ok(None));
        assert!(!guard.has_observed_raw_lt());
        assert_eq!(guard.buffered_len(), "safe prefix and private suffix".len());
        assert_eq!(
            guard.finish(),
            Ok(IncrementalAssistantTextResult::Plain(
                " and private suffix".into()
            ))
        );
    }

    #[test]
    fn incremental_guard_observes_own_lt_while_externally_frozen() {
        let mut guard = IncrementalAssistantTextGuard::new();
        guard.freeze_publication();
        assert_eq!(guard.push("before"), Ok(None));
        assert!(!guard.has_observed_raw_lt());
        assert_eq!(guard.push(" <"), Ok(None));
        assert!(guard.has_observed_raw_lt());
        assert_eq!(guard.scan_work_bytes(), "before <".len());

        assert_eq!(guard.push(" unscanned suffix"), Ok(None));
        assert_eq!(guard.scan_work_bytes(), "before <".len());
        assert_eq!(guard.released_cursor(), 0);
        assert_eq!(
            guard.finish(),
            Ok(IncrementalAssistantTextResult::Plain(
                "before < unscanned suffix".into()
            ))
        );
    }

    #[test]
    fn incremental_guard_never_releases_dsml_candidates_at_any_scalar_split() {
        let candidates = [
            concat!(
                " \n<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name=\"python\">",
                "<｜｜DSML｜｜parameter name=\"code\" string=true>super-secret",
                "</｜｜DSML｜｜parameter></｜｜DSML｜｜invoke>",
                "</｜｜DSML｜｜tool_calls>\t"
            ),
            concat!(
                "<||DSML||tool_calls><||DSML||invoke name='shell'>",
                "<||DSML||parameter name='command' string=true>private-body",
                "</||DSML||parameter></||DSML||invoke></||DSML||tool_calls>"
            ),
            concat!(
                "<||dsml||TOOL_CALLS><||DsMl||InVoKe name='x'>",
                "</||dSmL||iNvOkE></||DSML||tool_calls>"
            ),
            "<|｜DSML｜|tool_calls><｜|DSML|｜invoke name='x'>secret",
            "<｜｜DSML｜｜tool_calls><｜｜DSML｜｜invoke name='x'>secret",
            "</｜DSML｜tool_calls>secret",
        ];

        for source in candidates {
            let mut split_points = source
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            split_points.push(source.len());
            for split in split_points {
                let released = assert_incremental_guard_matches_parser(
                    source,
                    [&source[..split], &source[split..]],
                );
                assert!(
                    released.is_empty(),
                    "released candidate bytes at scalar split {split}: {source}"
                );
            }
        }
    }

    #[test]
    fn incremental_guard_keeps_whitespace_around_valid_dsml_private() {
        let source = concat!(
            " \n   <｜DSML｜tool_calls><｜DSML｜invoke name=\"list_skills\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>\r\n "
        );
        let (released, terminal) = run_incremental_guard(source, char_chunks(source));
        assert!(released.is_empty());
        let IncrementalAssistantTextResult::ToolCalls {
            visible_text,
            calls,
        } = terminal.unwrap()
        else {
            panic!("expected canonical calls");
        };
        assert!(visible_text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_skills");
        assert_eq!(calls[0].arguments, "{}");
    }

    #[test]
    fn incremental_guard_terminal_decision_matches_parser_context_rules() {
        let block = concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">",
            "<｜DSML｜parameter name=\"value\" string=true>secret",
            "</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        let cases = [
            "<｜DS",
            "<｜DSML｜tool_calls>",
            "<｜DSML｜unknown>",
            &format!("prose before {block}"),
            &format!("{block}\nprose after"),
            &format!("{block}{block}"),
            &format!("> {block}"),
            &format!("- {block}"),
            &format!("<div>\n{block}\n</div>"),
            &format!("```text\n{block}\n```"),
            &format!("`{block}`"),
            &format!("    {block}"),
            &format!("```text\ndocumentation\n{block}"),
            &format!("> ```text\n> documentation\n{block}\n> ```"),
            "<section>ordinary raw HTML</section>",
        ];

        for source in cases {
            assert_incremental_guard_matches_parser(source, char_chunks(source));
        }
    }

    #[test]
    fn incremental_guard_reconstructs_multibyte_markdown_exactly() {
        let cases = [
            "  **研究🚀** [链接](https://例子.test/路径)\n数学 $α + β$",
            "前缀🙂 [自动链接](<https://example.test/路径>) 后缀",
            "```text\n<not-dsml>文档示例</not-dsml>\n```\n尾部",
            "行内 `<not-dsml>🙂</not-dsml>` 与普通文本",
            "~~~rust\nfn main() { println!(\"你好\"); }\n~~~",
        ];

        for source in cases {
            assert_incremental_guard_matches_parser(source, char_chunks(source));
        }
    }

    #[test]
    fn incremental_guard_push_work_is_linear_and_size_bounded() {
        const LARGE_RESPONSE_BYTES: usize = 128 * 1024;
        let mut guard = IncrementalAssistantTextGuard::new();
        for _ in 0..LARGE_RESPONSE_BYTES {
            assert_eq!(guard.push("x"), Ok(Some("x".into())));
        }
        assert_eq!(guard.buffered_len(), LARGE_RESPONSE_BYTES);
        assert_eq!(guard.scan_work_bytes(), LARGE_RESPONSE_BYTES);
        assert_eq!(
            guard.finish(),
            Ok(IncrementalAssistantTextResult::Plain(String::new()))
        );

        let mut boundary = IncrementalAssistantTextGuard::new();
        let maximum = "x".repeat(MAX_ASSISTANT_TEXT_BYTES);
        assert_eq!(boundary.push(&maximum), Ok(Some(maximum.clone())));
        assert_eq!(boundary.scan_work_bytes(), MAX_ASSISTANT_TEXT_BYTES);
        assert_eq!(boundary.push("y"), Err(DsmlError::TooLarge));
        assert_eq!(boundary.buffered_len(), MAX_ASSISTANT_TEXT_BYTES);
        assert_eq!(boundary.scan_work_bytes(), MAX_ASSISTANT_TEXT_BYTES);
        assert_eq!(
            boundary.finish(),
            Ok(IncrementalAssistantTextResult::Plain(String::new()))
        );
    }

    #[test]
    fn parses_multiline_string_parameter_and_double_bars() {
        let source = concat!(
            "\n<｜｜DSML｜｜tool_calls>\n",
            "<｜｜DSML｜｜invoke name=\"python\">\n",
            "<｜｜DSML｜｜parameter name=\"code\" string=\"true\">",
            "# 研究检查\nprint(\"中子 \\\"flux\\\"\")\n",
            "</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n",
            "</｜｜DSML｜｜tool_calls>\n"
        );
        let ParsedAssistantText::ToolCalls {
            visible_text,
            calls,
        } = parse_assistant_text(source).unwrap()
        else {
            panic!("expected calls");
        };
        assert_eq!(visible_text, "");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "python");
        let args: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(
            args["code"],
            Value::String("# 研究检查\nprint(\"中子 \\\"flux\\\"\")\n".into())
        );
    }

    #[test]
    fn parses_multiple_calls_and_json_parameter_types() {
        let source = concat!(
            "<｜DSML｜tool_calls>",
            "<｜DSML｜invoke name=\"one\">",
            "<｜DSML｜parameter name=\"object\" string=false>{\"x\":1}</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"flag\" string=\"false\">true</｜DSML｜parameter>",
            "<｜DSML｜parameter name=\"nothing\" string=false>null</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "<｜DSML｜invoke name=\"two\">",
            "<｜DSML｜parameter name=\"count\" string=false>2</｜DSML｜parameter>",
            "</｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        let ParsedAssistantText::ToolCalls { calls, .. } = parse_assistant_text(source).unwrap()
        else {
            panic!("expected calls");
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "one");
        assert_eq!(calls[1].name, "two");
        let first: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(first["object"], serde_json::json!({"x": 1}));
        assert_eq!(first["flag"], true);
        assert!(first["nothing"].is_null());
    }

    #[test]
    fn supports_ascii_bars_and_unquoted_booleans() {
        let parsed = call(concat!(
            "<||DSML||tool_calls><||DSML||invoke name='shell'>",
            "<||DSML||parameter name='command' string=true>pwd</||DSML||parameter>",
            "</||DSML||invoke></||DSML||tool_calls>"
        ));
        assert_eq!(parsed.name, "shell");
        assert_eq!(
            serde_json::from_str::<Value>(&parsed.arguments).unwrap()["command"],
            "pwd"
        );
    }

    #[test]
    fn accepts_zero_parameter_invocation_as_empty_object() {
        let parsed = call(concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"list_skills\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        ));
        assert_eq!(parsed.name, "list_skills");
        assert_eq!(parsed.arguments, "{}");
    }

    #[test]
    fn preserves_complete_markdown_code_examples() {
        let source = concat!(
            "```text\n<｜DSML｜tool_calls>\n```\n",
            "~~~xml\n</｜DSML｜invoke>\n~~~\n",
            "`<||DSML||tool_calls>` and ``</｜DSML｜parameter>``\n",
            "> ```xml\n> <｜DSML｜tool_calls>\n> ````\n",
            "- ~~~xml\n  <｜DSML｜tool_calls>\n  ~~~~\n",
            "```text\n<｜DSML｜tool_calls>\n    ```\nstill fenced\n```"
        );
        assert_eq!(
            parse_assistant_text(source).unwrap(),
            ParsedAssistantText::Plain(source.into())
        );

        let inline_with_literal_backslash = concat!(
            "`<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>\\`"
        );
        assert_eq!(
            parse_assistant_text(inline_with_literal_backslash).unwrap(),
            ParsedAssistantText::Plain(inline_with_literal_backslash.into())
        );
    }

    #[test]
    fn ordinary_markdown_is_byte_for_byte_plain() {
        let source = "# 标题\n\n| a | b |\n|---|---|\n| 1 | $x^2$ |\n\n```python\nprint(1)\n```\n\n<DSMLDataset>science data</DSMLDataset>";
        assert_eq!(
            parse_assistant_text(source).unwrap(),
            ParsedAssistantText::Plain(source.into())
        );
    }

    #[test]
    fn shared_display_corpus_matches_backend_execution_contexts() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../../../test-fixtures/dsml-display-corpus.json"
        ))
        .unwrap();

        for source in corpus["plain"].as_array().unwrap() {
            let source = source.as_str().unwrap();
            assert_eq!(
                parse_assistant_text(source).unwrap(),
                ParsedAssistantText::Plain(source.into()),
                "plain corpus entry was not preserved: {source}"
            );
        }
        for (name, source) in corpus["regressions"].as_object().unwrap() {
            let source = source.as_str().unwrap();
            assert_eq!(
                parse_assistant_text(source),
                Err(DsmlError::InvalidContext),
                "regression `{name}` crossed the execution boundary"
            );
        }
    }

    #[test]
    fn preserves_dsml_inside_commonmark_indented_code() {
        let source = concat!(
            "    <｜DSML｜tool_calls><｜DSML｜invoke name=\"python\">\n",
            "    <｜DSML｜parameter name=\"code\" string=true>print(1)</｜DSML｜parameter>\n",
            "    </｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        assert_eq!(
            parse_assistant_text(source).unwrap(),
            ParsedAssistantText::Plain(source.into())
        );

        let after_blank = format!("Intro\n\n{source}");
        assert_eq!(
            parse_assistant_text(&after_blank).unwrap(),
            ParsedAssistantText::Plain(after_blank)
        );
    }

    #[test]
    fn invalid_fence_closer_cannot_authorize_following_protocol() {
        let source = concat!(
            "```text\nexample\n```still-code\n",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"></｜DSML｜invoke>",
            "</｜DSML｜tool_calls>"
        );
        assert_eq!(parse_assistant_text(source), Err(DsmlError::InvalidContext));
    }

    #[test]
    fn only_top_level_block_protocol_is_executable() {
        let call = concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        for source in [
            format!("- {call}"),
            format!("> {call}"),
            format!("prose {call}"),
            format!("prose before\n{call}"),
            format!("{call}\nprose after"),
            format!("<!-- {call} -->"),
            format!("<!--\n{call}\n-->"),
            format!("Intro\n    {call}"),
            format!("\\`{call}\\`"),
            format!("> ```text\n> documentation\n{call}\n> ```"),
            format!("- ```text\n  documentation\n{call}\n  ```"),
        ] {
            assert_eq!(
                parse_assistant_text(&source),
                Err(DsmlError::InvalidContext),
                "{source}"
            );
        }

        assert!(matches!(
            parse_assistant_text(&format!("   {call}")),
            Ok(ParsedAssistantText::ToolCalls { .. })
        ));
    }

    #[test]
    fn enforces_dsml_call_parameter_and_argument_boundaries() {
        let mut too_many_calls = String::from("<｜DSML｜tool_calls>");
        for index in 0..=MAX_TOOL_CALLS_PER_TURN {
            too_many_calls.push_str(&format!(
                "<｜DSML｜invoke name=\"t{index}\"></｜DSML｜invoke>"
            ));
        }
        too_many_calls.push_str("</｜DSML｜tool_calls>");
        assert_eq!(
            parse_assistant_text(&too_many_calls),
            Err(DsmlError::BoundaryExceeded)
        );

        let mut too_many_parameters =
            String::from("<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">");
        for index in 0..=MAX_TOOL_PARAMETERS_PER_CALL {
            too_many_parameters.push_str(&format!(
                "<｜DSML｜parameter name=\"p{index}\" string=true>x</｜DSML｜parameter>"
            ));
        }
        too_many_parameters.push_str("</｜DSML｜invoke></｜DSML｜tool_calls>");
        assert_eq!(
            parse_assistant_text(&too_many_parameters),
            Err(DsmlError::BoundaryExceeded)
        );

        let oversized_name = "n".repeat(MAX_TOOL_NAME_BYTES + 1);
        let oversized_tool_name = format!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"{oversized_name}\"></｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        assert_eq!(
            parse_assistant_text(&oversized_tool_name),
            Err(DsmlError::BoundaryExceeded)
        );

        let oversized_parameter_name = "p".repeat(MAX_TOOL_PARAMETER_NAME_BYTES + 1);
        let oversized_parameter = format!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜parameter name=\"{oversized_parameter_name}\" string=true>x</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        assert_eq!(
            parse_assistant_text(&oversized_parameter),
            Err(DsmlError::BoundaryExceeded)
        );

        let oversized_body = "x".repeat(MAX_TOOL_ARGUMENT_BYTES_PER_CALL + 1);
        let oversized_arguments = format!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜parameter name=\"code\" string=true>{oversized_body}</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        assert_eq!(
            parse_assistant_text(&oversized_arguments),
            Err(DsmlError::BoundaryExceeded)
        );

        let body = "x".repeat(400 * 1024);
        let mut oversized_total = String::from("<｜DSML｜tool_calls>");
        for index in 0..3 {
            oversized_total.push_str(&format!(
                "<｜DSML｜invoke name=\"t{index}\"><｜DSML｜parameter name=\"code\" string=true>{body}</｜DSML｜parameter></｜DSML｜invoke>"
            ));
        }
        oversized_total.push_str("</｜DSML｜tool_calls>");
        assert_eq!(
            parse_assistant_text(&oversized_total),
            Err(DsmlError::BoundaryExceeded)
        );
    }

    #[test]
    fn malformed_protocol_is_rejected_without_echoing_payload() {
        let cases = [
            "<｜DSML｜tool_calls>",
            "</｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls></｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls><｜DSML｜invoke><｜DSML｜parameter name=\"x\" string=true>secret</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜parameter name=\"a\" string=maybe>secret</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜parameter name=\"a\" string=false>not-json-secret</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜parameter name=\"a\" string=true>one</｜DSML｜parameter><｜DSML｜parameter name=\"a\" string=true>two</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>",
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"><｜DSML｜invoke name=\"nested\">",
            "<|｜DSML｜|tool_calls><｜|DSML|｜invoke name=\"x\"></｜|DSML|｜invoke></|｜DSML｜|tool_calls>",
            "```\n<｜DSML｜tool_calls>\n",
            "```text\n<｜DSML｜tool_calls>\n```not-a-close\n",
            "`<｜DSML｜tool_calls>",
        ];
        for source in cases {
            let error = parse_assistant_text(source).expect_err(source);
            assert!(!error.to_string().contains("secret"));
        }
    }

    #[test]
    fn truncated_real_protocol_prefixes_fail_closed() {
        for source in [
            "<｜",
            "<｜D",
            "<｜DS",
            "<｜DSML",
            "<｜｜DS",
            "<|D",
            "<|DSM",
            "<||DSM",
            "</｜DS",
            "<｜DS>",
        ] {
            assert_eq!(
                parse_assistant_text(source),
                Err(DsmlError::MalformedTag),
                "{source}"
            );
        }
    }

    #[test]
    fn adversarial_non_tags_and_unmatched_fences_remain_bounded() {
        let mut ordinary = "<x".repeat(100_000);
        ordinary.push_str("\n<DSMLDataset>still ordinary</DSMLDataset>");
        assert_eq!(
            parse_assistant_text(&ordinary).unwrap(),
            ParsedAssistantText::Plain(ordinary)
        );

        let mut unmatched_fences = "```still-open\n".repeat(20_000);
        unmatched_fences.push_str(concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        ));
        assert_eq!(
            parse_assistant_text(&unmatched_fences),
            Err(DsmlError::InvalidContext)
        );

        let mut unique_backticks = String::new();
        for width in 1..=1800 {
            unique_backticks.push_str(&"`".repeat(width));
            unique_backticks.push('x');
        }
        assert_eq!(
            parse_assistant_text(&unique_backticks).unwrap(),
            ParsedAssistantText::Plain(unique_backticks)
        );
    }

    #[test]
    fn rejects_multiple_blocks() {
        let block = concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">",
            "<｜DSML｜parameter name=\"a\" string=true>b</｜DSML｜parameter>",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        );
        assert_eq!(
            parse_assistant_text(&format!("{block}{block}")),
            Err(DsmlError::MultipleBlocks)
        );
    }

    #[test]
    fn rejects_oversized_turn() {
        let source = "x".repeat(MAX_ASSISTANT_TEXT_BYTES + 1);
        assert_eq!(parse_assistant_text(&source), Err(DsmlError::TooLarge));
    }
}
