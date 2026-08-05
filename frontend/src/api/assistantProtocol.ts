/**
 * Display-only guard for legacy assistant rows where a provider serialized its
 * tool protocol into ordinary text. This code never reconstructs or executes a
 * tool call; it only prevents untrusted protocol payloads from becoming
 * Markdown/DOM content.
 */

import { fromMarkdown } from "mdast-util-from-markdown";

export const HIDDEN_ASSISTANT_PROTOCOL_NOTICE =
  "> 已隐藏一段损坏的历史工具调用协议。";

/** Bound display work and fail closed for unexpectedly large provider output. */
export const MAX_ASSISTANT_DISPLAY_TEXT_LENGTH = 2 * 1024 * 1024;

const MAX_CONTROL_MARKER_LENGTH = 512;
const MAX_PROTOCOL_MARKDOWN_PARSE_LENGTH = 8 * 1024;
const MAX_MARKDOWN_CONTAINER_DEPTH = 32;
const MAX_MARKDOWN_DELIMITER_TOKENS = 1024;

type ControlName = "tool_calls" | "invoke" | "parameter" | "unknown";

interface Range {
  start: number;
  end: number;
}

interface ControlMarker extends Range {
  closing: boolean;
  name: ControlName;
  partial: boolean;
}

type DisplaySegment =
  | { kind: "text"; value: string }
  | { kind: "notice" };

function isProtocolBar(character: string | undefined): boolean {
  return character === "｜" || character === "|";
}

/**
 * Constant-lookahead guard before the bounded marker scan. Ordinary Markdown
 * may contain millions of '<' characters; only a '<' followed by the actual
 * DSML bar/prefix shape can possibly start control text.
 */
function hasProtocolCandidatePrefix(text: string, open: number): boolean {
  let cursor = open + 1;
  if (text[cursor] === "/") cursor += 1;

  let bars = 0;
  while (isProtocolBar(text[cursor])) {
    bars += 1;
    cursor += 1;
    if (bars > 2) return true;
  }
  if (bars === 0) return false;

  let probe = "";
  while (probe.length < 4 && /[A-Za-z]/.test(text[cursor] ?? "")) {
    probe += text[cursor];
    cursor += 1;
  }
  return "DSML".startsWith(probe.toUpperCase());
}

function containsProtocolCandidate(text: string): boolean {
  let cursor = 0;
  for (;;) {
    const open = text.indexOf("<", cursor);
    if (open < 0) return false;
    if (hasProtocolCandidatePrefix(text, open)) return true;
    cursor = open + 1;
  }
}

/**
 * micromark deliberately supports nested containers, but adversarial quote
 * prefixes can make exact parsing disproportionately expensive. This linear
 * preflight rejects depths far beyond useful scientific Markdown before the
 * synchronous parser reaches the UI thread.
 */
function exceedsMarkdownComplexityBudget(text: string): boolean {
  let lineStart = 0;
  let delimiterTokens = 0;
  while (lineStart < text.length) {
    const newline = text.indexOf("\n", lineStart);
    const lineEnd = newline < 0 ? text.length : newline;
    let cursor = lineStart;
    let containerDepth = 0;
    for (;;) {
      const checkpoint = cursor;
      let spaces = 0;
      while (spaces < 3 && cursor < lineEnd && text[cursor] === " ") {
        cursor += 1;
        spaces += 1;
      }
      if (text[cursor] === ">") {
        cursor += 1;
        if (text[cursor] === " " || text[cursor] === "\t") cursor += 1;
      } else {
        const unordered = text[cursor] === "-" || text[cursor] === "+" || text[cursor] === "*";
        const ordered = text.slice(cursor, lineEnd).match(/^\d{1,9}[.)]/)?.[0];
        const markerWidth = unordered ? 1 : ordered?.length ?? 0;
        if (markerWidth === 0) {
          cursor = checkpoint;
          break;
        }
        cursor += markerWidth;
        let separatorWidth = 0;
        while (
          separatorWidth < 4 &&
          cursor < lineEnd &&
          (text[cursor] === " " || text[cursor] === "\t")
        ) {
          cursor += 1;
          separatorWidth += 1;
        }
        if (separatorWidth === 0) {
          cursor = checkpoint;
          break;
        }
      }
      containerDepth += 1;
      if (containerDepth > MAX_MARKDOWN_CONTAINER_DEPTH) return true;
    }
    if (newline < 0) break;
    lineStart = newline + 1;
  }
  for (const character of text) {
    if ("`*_~[]!".includes(character)) {
      delimiterTokens += 1;
      if (delimiterTokens > MAX_MARKDOWN_DELIMITER_TOKENS) return true;
    }
  }
  return false;
}

function exactControlName(token: string): ControlName {
  switch (token.toLowerCase()) {
    case "tool_calls":
      return "tool_calls";
    case "invoke":
      return "invoke";
    case "parameter":
      return "parameter";
    default:
      return "unknown";
  }
}

/**
 * Parse the inside of a DSML tag. A real marker must use the provider's bar
 * delimiters on both sides of DSML, so ordinary near-tags such as
 * `<DSMLDataset>` are never treated as control-plane text.
 */
function parseMarkerBody(
  value: string,
  _partial: boolean,
): { closing: boolean; name: ControlName } | null {
  let cursor = 0;
  const closing = value[cursor] === "/";
  if (closing) cursor += 1;

  const barsStart = cursor;
  while (isProtocolBar(value[cursor])) cursor += 1;
  const leadingBars = cursor - barsStart;
  if (leadingBars === 0) return null;
  // The backend grammar accepts one or two bars. Three or more are a DSML
  // candidate but malformed, so the display boundary must fail closed too.
  if (leadingBars > 2) return { closing, name: "unknown" };

  const headEnd = value.slice(cursor).search(/\s/);
  const head = value.slice(cursor, headEnd < 0 ? value.length : cursor + headEnd);
  if (head.length < 4) {
    return "DSML".startsWith(head.toUpperCase()) ? { closing, name: "unknown" } : null;
  }
  if (head.slice(0, 4).toUpperCase() !== "DSML") return null;
  cursor += 4;

  // Mixed ASCII/full-width bars are intentionally valid, matching
  // `take_protocol_bars` in the backend; each side is still limited to 1–2.
  const afterDsml = head.slice(4);
  let trailingBars = 0;
  while (isProtocolBar(afterDsml[trailingBars])) trailingBars += 1;
  if (trailingBars < 1 || trailingBars > 2) return { closing, name: "unknown" };
  return { closing, name: exactControlName(afterDsml.slice(trailingBars)) };
}

function parseCompleteMarker(text: string, start: number, end: number): ControlMarker | null {
  const parsed = parseMarkerBody(text.slice(start + 1, end - 1), false);
  return parsed ? { start, end, ...parsed, partial: false } : null;
}

function parsePartialMarker(text: string, start: number, bodyEnd: number): ControlMarker | null {
  const parsed = parseMarkerBody(text.slice(start + 1, bodyEnd), true);
  return parsed ? { start, end: text.length, ...parsed, partial: true } : null;
}

interface PositionedMarkdownNode {
  type: string;
  position?: {
    start?: { offset?: number };
    end?: { offset?: number };
  };
  children?: PositionedMarkdownNode[];
}

/**
 * Parse the small subset of a fence prefix we intentionally preserve as
 * documentation. Tabs and nested lists are rejected conservatively; hiding an
 * ambiguous example is safer than allowing a hand-written container parser to
 * disagree with CommonMark and expose provider control text.
 */
function openingFenceContainer(prefix: string): { quoteDepth: number; listIndent: number | null } | null {
  if (prefix.includes("\t")) return null;
  let cursor = 0;
  let quoteDepth = 0;
  for (;;) {
    const checkpoint = cursor;
    let spaces = 0;
    while (spaces < 3 && prefix[cursor] === " ") {
      cursor += 1;
      spaces += 1;
    }
    if (prefix[cursor] !== ">") {
      cursor = checkpoint;
      break;
    }
    cursor += 1;
    if (prefix[cursor] === " ") cursor += 1;
    quoteDepth += 1;
  }

  const remainder = prefix.slice(cursor);
  if (/^ {0,3}$/.test(remainder)) return { quoteDepth, listIndent: null };
  if (/^ {0,3}(?:[-+*]|\d{1,9}[.)]) {1,4}$/.test(remainder)) {
    return { quoteDepth, listIndent: remainder.length };
  }
  return null;
}

function closingFenceContainer(
  prefix: string,
  opening: { quoteDepth: number; listIndent: number | null },
): boolean {
  if (prefix.includes("\t")) return false;
  let cursor = 0;
  for (let depth = 0; depth < opening.quoteDepth; depth += 1) {
    let spaces = 0;
    while (spaces < 3 && prefix[cursor] === " ") {
      cursor += 1;
      spaces += 1;
    }
    if (prefix[cursor] !== ">") return false;
    cursor += 1;
    if (prefix[cursor] === " ") cursor += 1;
  }
  const remainder = prefix.slice(cursor);
  if (!/^ *$/.test(remainder)) return false;
  if (opening.listIndent === null) return remainder.length <= 3;
  return remainder.length >= opening.listIndent && remainder.length <= opening.listIndent + 3;
}

function physicalLineStart(text: string, offset: number): number {
  let cursor = offset;
  while (cursor > 0 && text[cursor - 1] !== "\n") cursor -= 1;
  return cursor;
}

function hasExplicitFenceCloser(text: string, start: number, end: number): boolean {
  const marker = text[start];
  if (marker !== "`" && marker !== "~") return true;
  let openingEnd = start;
  while (text[openingEnd] === marker) openingEnd += 1;
  const openingWidth = openingEnd - start;
  if (openingWidth < 3) return true;

  const openingPrefix = text.slice(physicalLineStart(text, start), start);
  const opening = openingFenceContainer(openingPrefix);
  if (!opening) return false;

  const closingLineStart = physicalLineStart(text, end);
  let closingEnd = end;
  while (closingEnd > closingLineStart && /[ \t\r]/.test(text[closingEnd - 1])) {
    closingEnd -= 1;
  }
  let closingStart = closingEnd;
  while (closingStart > closingLineStart && text[closingStart - 1] === marker) {
    closingStart -= 1;
  }
  if (closingEnd - closingStart < openingWidth) return false;
  return closingFenceContainer(text.slice(closingLineStart, closingStart), opening);
}

/**
 * Use the exact CommonMark parser that backs react-markdown. Only AST code
 * nodes are protected, so backticks cannot cross blank lines/headings and an
 * escaped-looking closer inside an open span still closes it. Fenced nodes also
 * require a positively verified explicit closer; incomplete fences fail closed.
 */
function protectedCodeRanges(text: string): Range[] {
  const root = fromMarkdown(text) as PositionedMarkdownNode;
  const ranges: Range[] = [];
  const pending: PositionedMarkdownNode[] = [root];
  while (pending.length > 0) {
    const node = pending.pop()!;
    if (node.type === "inlineCode" || node.type === "code") {
      const start = node.position?.start?.offset;
      const end = node.position?.end?.offset;
      if (
        typeof start === "number" &&
        typeof end === "number" &&
        start < end &&
        (node.type === "inlineCode" || hasExplicitFenceCloser(text, start, end))
      ) {
        ranges.push({ start, end });
      }
      continue;
    }
    if (node.children) pending.push(...node.children);
  }
  return ranges.sort((left, right) => left.start - right.start || left.end - right.end);
}

function rangeContaining(ranges: Range[], offset: number): Range | undefined {
  let low = 0;
  let high = ranges.length - 1;
  while (low <= high) {
    const middle = (low + high) >> 1;
    const range = ranges[middle];
    if (offset < range.start) high = middle - 1;
    else if (offset >= range.end) low = middle + 1;
    else return range;
  }
  return undefined;
}

function findNextControlMarker(
  text: string,
  from: number,
  protectedRanges: Range[],
): ControlMarker | null {
  let cursor = from;
  while (cursor < text.length) {
    const open = text.indexOf("<", cursor);
    if (open < 0) return null;
    const protectedRange = rangeContaining(protectedRanges, open);
    if (protectedRange) {
      cursor = protectedRange.end;
      continue;
    }
    if (!hasProtocolCandidatePrefix(text, open)) {
      cursor = open + 1;
      continue;
    }

    // Protocol headers are short. Bounding this scan prevents many unmatched
    // '<' characters from turning marker discovery quadratic.
    const scanLimit = Math.min(text.length, open + MAX_CONTROL_MARKER_LENGTH);
    let markerEnd = open + 1;
    while (
      markerEnd < scanLimit &&
      text[markerEnd] !== ">" &&
      text[markerEnd] !== "\n" &&
      text[markerEnd] !== "\r"
    ) {
      markerEnd += 1;
    }
    if (text[markerEnd] === ">") {
      const marker = parseCompleteMarker(text, open, markerEnd + 1);
      if (marker) return marker;
    } else {
      const partial = parsePartialMarker(text, open, markerEnd);
      if (partial) return partial;
    }
    cursor = open + 1;
  }
  return null;
}

type BlockResult =
  | { kind: "complete"; end: number }
  | { kind: "unsafe" };

function findCompleteToolBlock(
  text: string,
  opening: ControlMarker,
  protectedRanges: Range[],
): BlockResult {
  const stack: ControlName[] = ["tool_calls"];
  let cursor = opening.end;

  for (;;) {
    const marker = findNextControlMarker(text, cursor, protectedRanges);
    if (!marker || marker.partial || marker.name === "unknown") return { kind: "unsafe" };
    const parent = stack[stack.length - 1];
    if (marker.closing) {
      if (marker.name !== parent) return { kind: "unsafe" };
      stack.pop();
      if (stack.length === 0) return { kind: "complete", end: marker.end };
    } else {
      const validChild =
        (parent === "tool_calls" && marker.name === "invoke") ||
        (parent === "invoke" && marker.name === "parameter");
      if (!validChild) return { kind: "unsafe" };
      stack.push(marker.name);
    }
    cursor = marker.end;
  }
}

function renderSegments(segments: DisplaySegment[]): string {
  let rendered = "";
  let previousWasNotice = false;
  for (const segment of segments) {
    if (segment.kind === "notice") {
      if (rendered.length > 0 && !rendered.endsWith("\n\n")) {
        rendered += rendered.endsWith("\n") ? "\n" : "\n\n";
      }
      rendered += HIDDEN_ASSISTANT_PROTOCOL_NOTICE;
      previousWasNotice = true;
      continue;
    }

    if (previousWasNotice && segment.value.length > 0 && !segment.value.startsWith("\n\n")) {
      rendered += segment.value.startsWith("\n") ? "\n" : "\n\n";
    }
    rendered += segment.value;
    previousWasNotice = false;
  }
  return rendered;
}

/**
 * Hide provider control-plane text before it reaches Markdown rendering.
 *
 * Complete blocks retain trusted prose before/after them. Once the structure is
 * malformed or partial, the suffix cannot be proven to be outside a parameter
 * body and is dropped. An orphan close tag makes the whole assistant text
 * untrustworthy. Complete fenced, inline, and CommonMark-indented code examples
 * remain byte-for-byte unchanged.
 */
export function sanitizeAssistantDisplayText(text: string): string {
  if (text.length === 0) return text;
  if (text.length > MAX_ASSISTANT_DISPLAY_TEXT_LENGTH) {
    return HIDDEN_ASSISTANT_PROTOCOL_NOTICE;
  }
  if (!containsProtocolCandidate(text)) return text;
  // Exact CommonMark parsing is reserved for bounded candidate-bearing text.
  // Ordinary large reports bypass it, while oversized control-like output is
  // quarantined instead of creating a parser-level denial of service.
  if (text.length > MAX_PROTOCOL_MARKDOWN_PARSE_LENGTH) {
    return HIDDEN_ASSISTANT_PROTOCOL_NOTICE;
  }
  if (exceedsMarkdownComplexityBudget(text)) {
    return HIDDEN_ASSISTANT_PROTOCOL_NOTICE;
  }
  const protectedRanges = protectedCodeRanges(text);
  const segments: DisplaySegment[] = [];
  let cursor = 0;

  for (;;) {
    const marker = findNextControlMarker(text, cursor, protectedRanges);
    if (!marker) {
      segments.push({ kind: "text", value: text.slice(cursor) });
      return renderSegments(segments);
    }

    if (marker.closing) return HIDDEN_ASSISTANT_PROTOCOL_NOTICE;
    segments.push({ kind: "text", value: text.slice(cursor, marker.start) });
    if (marker.partial || marker.name !== "tool_calls") {
      segments.push({ kind: "notice" });
      return renderSegments(segments);
    }

    const block = findCompleteToolBlock(text, marker, protectedRanges);
    segments.push({ kind: "notice" });
    if (block.kind === "unsafe") return renderSegments(segments);
    cursor = block.end;
  }
}
