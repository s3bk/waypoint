// Breaking a node title into lines. Pure (the caller supplies the text
// measurement), so it can be tested without a browser.
//
// - Manual line breaks (\n) in the title are always kept.
// - Within a paragraph the break is chosen by a small cost model: lines should
//   be balanced, and a break is cheap after punctuation or before a
//   conjunction/preposition ("Migrate the database | to Postgres") and
//   expensive after an article or preposition ("Migrate the | database").
// - How wide a line may be depends on the shape: an ellipse or rhombus is
//   narrower near its top and bottom than at its middle.
// - Text that still does not fit ends in an ellipsis on the last line.

export type Measure = (text: string) => number;

export const LINE_HEIGHT = 17;
export const MAX_LINES = 2;

/** Half the width of the shape at vertical distance `dy` from its centre. */
function halfWidthAt(shape: string, w: number, h: number, dy: number): number {
  const y = Math.min(Math.abs(dy), h / 2);
  switch (shape) {
    case "ellipse":
      return (w / 2) * Math.sqrt(Math.max(0, 1 - (y / (h / 2)) ** 2));
    case "rhombus":
      return (w / 2) * (1 - y / (h / 2));
    case "hexagon":
      return w / 2 - y;
    case "pill": {
      const r = h / 2;
      return w / 2 - r + Math.sqrt(Math.max(0, r * r - y * y));
    }
    default:
      return w / 2;
  }
}

/** The widest a line may be when the title takes `lines` lines, centred in a `w` by `h` shape. */
export function textWidth(shape: string, w: number, h: number, lines: number): number {
  // The outermost glyph edge: half the block, less the empty space above/below the letters in a line box.
  const extent = Math.max(0, (lines * LINE_HEIGHT) / 2 - 3);
  const pad = shape === "rhombus" ? 6 : 12;
  return Math.max(24, 2 * halfWidthAt(shape, w, h, extent) - 2 * pad);
}

interface Item {
  text: string;
  /** Part of a long word that had to be split: joined to the previous item without a space. */
  glued: boolean;
  /** Cost of breaking before this item, when glued. */
  cost: number;
  /** Starts a manual paragraph: a break is forced before it. */
  hard: boolean;
}

// Words a line should not end with: they belong to what follows.
const BINDS_NEXT = new Set(
  "a an the this these those my your our its their his her of to in on at by for with from into onto over under via per as than and or but nor if so not is are was be no vs between about after before".split(" "),
);
// Words a line is good to start with: a new phrase begins here.
const STARTS_PHRASE = new Set(
  "and or but nor with for to in on at by from into via per when while where if because that which so before after until without using vs than between about over under as".split(" "),
);

const bare = (s: string) => s.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, "").toLowerCase();

/** The cost of a line break between two items; lower is better, negative is a good place. */
function breakCost(prev: Item, next: Item): number {
  if (next.glued) return next.cost;
  const p = prev.text;
  const last = p[p.length - 1]!;
  const word = bare(p);
  let c = 0;
  const punctuated = /[,;:.!?—–)\]]$/.test(p);
  if (",;:".includes(last)) c -= 2.5;
  else if (".!?".includes(last)) c -= 1.5;
  else if ("—–".includes(last) || p === "-") c -= 2;
  else if (")]".includes(last)) c -= 1;
  if (!punctuated) {
    if (BINDS_NEXT.has(word)) c += 6; // "the", "of", "and" ... dangling at the end of a line
    else if (word.length <= 2 && !/\d/.test(word)) c += 1.5; // any other very short word
  }
  if (p.endsWith("(")) c += 6;
  if (next.text.startsWith("(")) c -= 1.5;
  if (STARTS_PHRASE.has(bare(next.text))) c -= 1.5;
  // Keep a number with what it counts: "3 days", "Q3 2026".
  if (/^[#$€£]?\d[\d.,:%]*$/.test(p)) c += 3;
  return c;
}

/** Positions where a word may be split if it is too wide on its own: after `-_/.:`, and at camelCase. */
function softPieces(word: string): string[] {
  const out: string[] = [];
  let cur = "";
  for (let i = 0; i < word.length; i++) {
    const ch = word[i]!;
    if (cur && /[a-z0-9]/.test(cur[cur.length - 1]!) && /[A-Z]/.test(ch)) {
      out.push(cur);
      cur = "";
    }
    cur += ch;
    if ("-_/.:".includes(ch) && i < word.length - 1) {
      out.push(cur);
      cur = "";
    }
  }
  if (cur) out.push(cur);
  return out;
}

function charPieces(piece: string, width: number, measure: Measure): string[] {
  const out: string[] = [];
  let cur = "";
  for (const ch of piece) {
    if (cur && measure(cur + ch) > width) {
      out.push(cur);
      cur = ch;
    } else cur += ch;
  }
  if (cur) out.push(cur);
  return out;
}

function toItems(paragraphs: string[], width: number, measure: Measure): Item[] {
  const items: Item[] = [];
  paragraphs.forEach((para, pi) => {
    let first = true;
    for (const word of para.split(/\s+/)) {
      if (!word) continue;
      const hard = first && pi > 0;
      first = false;
      if (measure(word) <= width) {
        items.push({ text: word, glued: false, cost: 0, hard });
        continue;
      }
      let atStart = true;
      for (const soft of softPieces(word)) {
        const pieces = measure(soft) <= width ? [soft] : charPieces(soft, width, measure);
        pieces.forEach((text, k) => {
          // Only the very first piece of the word starts a new item; the rest are joined without a space.
          // Breaking at a soft point (`-`, `_`, a capital) is cheaper than splitting inside a piece.
          items.push({ text, glued: !atStart, cost: k > 0 ? 7 : 2.5, hard: hard && atStart });
          atStart = false;
        });
      }
    }
  });
  return items;
}

const joinItems = (items: Item[], from: number, to: number): string => {
  let s = "";
  for (let k = from; k < to; k++) s += (k > from && !items[k]!.glued ? " " : "") + items[k]!.text;
  return s;
};

const slackCost = (width: number, max: number) => 8 * ((max - width) / max) ** 2;
const LINE_COST = 2.5; // each extra line has to earn its keep

/** One paragraph (items with no forced break inside) split into the cheapest lines. */
function wrapParagraph(items: Item[], max: number, measure: Measure): string[] {
  const m = items.length;
  if (m === 0) return [];
  const whole = joinItems(items, 0, m);
  if (measure(whole) <= max) return [whole];
  const best = new Array<number>(m + 1).fill(Infinity);
  const from = new Array<number>(m + 1).fill(0);
  best[0] = 0;
  for (let j = 1; j <= m; j++) {
    let line = "";
    for (let i = j - 1; i >= 0; i--) {
      line = i === j - 1 ? items[i]!.text : items[i]!.text + (items[i + 1]!.glued ? "" : " ") + line;
      const w = measure(line);
      if (w > max && i < j - 1) break; // only gets wider
      if (best[i] === Infinity) continue;
      let cost = best[i]! + slackCost(Math.min(w, max), max) + LINE_COST + (i > 0 ? breakCost(items[i - 1]!, items[i]!) : 0);
      // A last line that is a lone short word looks like a mistake.
      if (j === m && j - i === 1 && !items[i]!.glued && items[i]!.text.length <= 3 && m > 1) cost += 3;
      if (cost < best[j]!) {
        best[j] = cost;
        from[j] = i;
      }
    }
  }
  const lines: string[] = [];
  for (let j = m; j > 0; j = from[j]!) lines.unshift(joinItems(items, from[j]!, j));
  return lines;
}

function ellipsize(text: string, max: number, measure: Measure): string {
  let s = text.slice(0, Math.max(1, Math.ceil(max / 3))).trimEnd();
  while (s && measure(`${s}…`) > max) s = s.slice(0, -1).trimEnd();
  return `${s}…`;
}

/** Too much text for the lines allowed: fill lines one by one (still preferring good breaks), and end the last with "…". */
function truncate(items: Item[], max: number, lines: number, measure: Measure): string[] {
  const out: string[] = [];
  let i = 0;
  while (i < items.length && out.length < lines - 1) {
    let pick = i + 1;
    let pickScore = Infinity;
    for (let e = i + 1; e <= items.length; e++) {
      const w = measure(joinItems(items, i, e));
      if (w > max && e > i + 1) break;
      const forced = e < items.length && items[e]!.hard;
      // The text is cut anyway, so fill the line rather than chase a perfect break.
      const score = (e < items.length ? breakCost(items[e - 1]!, items[e]!) * 0.5 : 0) + 3 * slackCost(Math.min(w, max), max);
      if (forced || score < pickScore) {
        pick = e;
        pickScore = score;
      }
      if (forced) break;
    }
    out.push(joinItems(items, i, pick));
    i = pick;
  }
  if (i < items.length) {
    // The last line holds the rest of the current paragraph; a later paragraph is cut off, so mark it.
    let end = i + 1;
    while (end < items.length && !items[end]!.hard) end++;
    const rest = joinItems(items, i, end);
    out.push(end === items.length && measure(rest) <= max ? rest : ellipsize(rest, max, measure));
  }
  return out;
}

/**
 * The lines to draw for `title` in a node of `shape` and size `w` by `h`:
 * as few as fit (at most MAX_LINES), manual breaks kept.
 */
export function layoutTitle(title: string, shape: string, w: number, h: number, measure: Measure): string[] {
  const paragraphs = title
    .split("\n")
    .map((p) => p.trim())
    .filter(Boolean);
  if (paragraphs.length === 0) return [];
  for (let n = 1; n <= MAX_LINES; n++) {
    const max = textWidth(shape, w, h, n);
    const items = toItems(paragraphs, max, measure);
    // Split at the forced breaks, wrap each paragraph on its own.
    const groups: Item[][] = [];
    for (const it of items) {
      if (it.hard || groups.length === 0) groups.push([]);
      groups[groups.length - 1]!.push(it);
    }
    const lines = groups.flatMap((g) => wrapParagraph(g, max, measure));
    if (lines.length <= n) return lines;
    if (n === MAX_LINES) return truncate(items, max, n, measure);
  }
  return [];
}
