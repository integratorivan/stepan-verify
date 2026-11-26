// «Изменения»: render changes.json (two Mermaid diagrams, «Было» and «Стало») with Pele into PNGs
// for the app, and write layout.json with the box of every block so the app can make them clickable.
//
//   node render.mjs <dir> <fonts-dir>
//
// Writes into <dir>: {before,after}-{light,dark}{,-plain}.png (2x, transparent) and layout.json:
//   { before: { w, h, stats, blocks: { <id>: { x, y, w, h, status } } }, after: { … } }
// Sizes are in points (1x). The diff is computed from the diagrams themselves: a block id only on
// one side is added / removed, the same id with other text is changed; links and messages are
// compared by their ends (flowcharts) or by their label text (other types).
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const [dir, fontsDir] = process.argv.slice(2);
if (!dir) {
  console.error('usage: node render.mjs <dir> <fonts-dir>');
  process.exit(2);
}
const CACHE = join(homedir(), '.cache', 'pr-before-after');
const PELE_VERSION = '1.13.0';

function fail(message) {
  console.error(message);
  process.exit(1);
}

function loadChromium() {
  for (const base of [join(CACHE, 'noop.js'), join(process.cwd(), 'noop.js')]) {
    try {
      return createRequire(base)('playwright').chromium;
    } catch {}
  }
  fail(`Нет playwright. Поставь его один раз: mkdir -p ${CACHE} && cd ${CACHE} && npm i playwright && npx playwright install chromium`);
}

async function peleBundle() {
  const cached = join(CACHE, `pele-${PELE_VERSION}.global.js`);
  if (existsSync(cached)) return cached;
  const response = await fetch(`https://cdn.jsdelivr.net/npm/pele@${PELE_VERSION}/dist/pele.global.js`);
  if (!response.ok) fail(`Не удалось скачать Pele: HTTP ${response.status}`);
  mkdirSync(CACHE, { recursive: true });
  writeFileSync(cached, Buffer.from(await response.arrayBuffer()));
  return cached;
}

function fontFaces() {
  if (!fontsDir || !existsSync(fontsDir)) return '';
  const weight = { Regular: 400, Medium: 500, SemiBold: 600, Bold: 700 };
  return readdirSync(fontsDir)
    .map((f) => [f, /^InstrumentSans-(Regular|Medium|SemiBold|Bold)\.ttf$/.exec(f)?.[1]])
    .filter(([, w]) => w)
    .map(([f, w]) => `@font-face{font-family:"Instrument Sans";font-weight:${weight[w]};src:url(data:font/ttf;base64,${readFileSync(join(fontsDir, f)).toString('base64')}) format("truetype")}`)
    .join('\n');
}

// Stepan's palette (ui.rs), light and dark.
const CSS = `
body { margin: 0; background: transparent; }
body > div { display: inline-block; padding: 0; }
body.light {
  --fg: #23251D; --muted: #65675E; --surface: #FDFDF8; --surface2: #F5F5EF; --line: #8E9087; --border: #C6C7BE; --accent: #F54E00;
  --green: #2F9E57; --green-soft: #DCF1E2; --green-text: #1E7A40;
  --red: #E0352B; --red-soft: #FCE2DE; --red-text: #B3241B;
  --amber: #F1A82C; --amber-soft: #FFF0CC; --amber-text: #8A5800; --amber-badge: #C98A12;
}
body.dark {
  --fg: #E9E9E2; --muted: #A2A49B; --surface: #2C2E34; --surface2: #25272C; --line: #7C7E86; --border: #4A4C54; --accent: #FF6A1F;
  --green: #4CC57A; --green-soft: #173323; --green-text: #6FD898;
  --red: #FF5A4E; --red-soft: #3D1E1C; --red-text: #FF8A80;
  --amber: #F5B544; --amber-soft: #3A2E14; --amber-text: #F5C46A; --amber-badge: #B07A10;
}
svg.pele {
  --pele-bg: transparent; --pele-fg: var(--fg); --pele-muted: var(--muted); --pele-line: var(--line);
  --pele-surface: var(--surface); --pele-surface-alt: var(--surface2); --pele-border: var(--border); --pele-accent: var(--accent);
  --pele-font: "Instrument Sans", -apple-system, sans-serif; --pele-font-mono: "SF Mono", Menlo, monospace; --pele-radius: 7px;
  background: transparent !important;
}
svg.pele > rect:first-child { fill: transparent !important; }
svg.pele .badge { display: none; }
svg.pele .badge rect { stroke: none; }
svg.pele .badge text { fill: #fff; stroke: none; font: 600 10px/1 "Instrument Sans", sans-serif; dominant-baseline: middle; letter-spacing: .02em; }
.diff svg.pele .badge { display: inline; }
.diff svg.pele .badge-added rect { fill: var(--green); }
.diff svg.pele .badge-removed rect { fill: var(--red); }
.diff svg.pele .badge-changed rect { fill: var(--amber-badge); }
.diff svg.pele .pele-node[data-st=added] > :is(rect, polygon, circle, ellipse, path):not(.pele-marker) { fill: var(--green-soft); stroke: var(--green); stroke-width: 1.6px; }
.diff svg.pele .pele-node[data-st=added] > text, .diff svg.pele .pele-node[data-st=added] text:not(.badge text) { fill: var(--green-text); font-weight: 600; }
.diff svg.pele .pele-node[data-st=removed] > :is(rect, polygon, circle, ellipse, path):not(.pele-marker) { fill: var(--red-soft); stroke: var(--red); stroke-width: 1.6px; stroke-dasharray: 5 4; }
.diff svg.pele .pele-node[data-st=removed] text:not(.badge text) { fill: var(--red-text); font-weight: 600; text-decoration: line-through; }
.diff svg.pele .pele-node[data-st=changed] > :is(rect, polygon, circle, ellipse, path):not(.pele-marker) { fill: var(--amber-soft); stroke: var(--amber); stroke-width: 1.6px; }
.diff svg.pele .pele-node[data-st=changed] text:not(.badge text) { fill: var(--amber-text); font-weight: 600; }
.diff svg.pele .pele-edge[data-st=added] path:not(.pele-marker) { stroke: var(--green); stroke-width: 2.2px; }
.diff svg.pele .pele-edge[data-st=added] .pele-marker { fill: var(--green); stroke: var(--green); }
.diff svg.pele .pele-edge[data-st=removed] path:not(.pele-marker) { stroke: var(--red); stroke-width: 2px; stroke-dasharray: 6 5; }
.diff svg.pele .pele-edge[data-st=removed] .pele-marker { fill: var(--red); stroke: var(--red); }
.diff svg.pele .pele-edge-label[data-st=added] text { fill: var(--green-text); font-weight: 600; }
.diff svg.pele .pele-edge-label[data-st=removed] text { fill: var(--red-text); font-weight: 600; text-decoration: line-through; }
.diff svg.pele .pele-note[data-st=added] rect { fill: var(--green-soft); stroke: var(--green); }
.diff svg.pele .pele-note[data-st=removed] rect { fill: var(--red-soft); stroke: var(--red); stroke-dasharray: 5 4; }
`;

// ------------------------------------------------------------------ in page
function renderBoth({ before, after, changed }) {
  const ns = 'http://www.w3.org/2000/svg';
  const FONT = '"Instrument Sans", -apple-system, sans-serif';
  const BADGE = { added: 'новое', removed: 'убрано', changed: 'изменено' };

  function draw(id, src) {
    const holder = document.createElement('div');
    holder.id = id;
    holder.innerHTML = pele.render(src, { fontFamily: FONT, fontSize: 14, idPrefix: `${id}-`, autoDirection: false }).svg;
    document.body.append(holder);
    const svg = holder.querySelector('svg');
    svg.classList.add('pele');
    // Sequence diagrams repeat the participants at the bottom: the top row is enough.
    svg.querySelectorAll('.pele-actor-footer').forEach((n) => n.remove());
    return svg;
  }
  const isFlow = (src) => /^\s*(flowchart|graph)\b/.test(src);
  const text = (el) => [...el.querySelectorAll('text')].map((t) => t.textContent.trim()).join('|');
  function blocks(svg) {
    const m = new Map();
    for (const g of svg.querySelectorAll('.pele-node[data-id]')) if (!m.has(g.dataset.id)) m.set(g.dataset.id, text(g));
    return m;
  }
  function edgeKeys(svg, src) {
    // id → comparable key: ends for flowcharts, label text otherwise.
    const keys = new Map();
    if (isFlow(src)) {
      for (const e of pele.parse(src).edges) keys.set(e.id, `${e.start}>${e.end}`);
    } else {
      for (const l of svg.querySelectorAll('.pele-edge-label[data-id], .pele-note[data-id]')) {
        const t = l.textContent.trim();
        if (t) keys.set(l.dataset.id, `t:${t}`);
      }
    }
    return keys;
  }

  const sb = draw('before', before);
  const sa = draw('after', after);
  const [bb, ba] = [blocks(sb), blocks(sa)];
  const [eb, ea] = [edgeKeys(sb, before), edgeKeys(sa, after)];
  if (!bb.size && !ba.size) return { error: 'pele не нашёл ни одного блока: проверь синтаксис Mermaid' };

  function mark(svg, mine, other, myEdges, otherEdges, gone) {
    const st = new Map([...mine].map(([id, t]) => [id, !other.has(id) ? gone : other.get(id) !== t || changed.includes(id) ? 'changed' : 'same']));
    for (const g of svg.querySelectorAll('.pele-node[data-id]')) {
      const s = st.get(g.dataset.id) ?? 'same';
      g.dataset.st = s;
      if (s === 'same' || g.querySelector('.badge')) continue;
      const box = g.getBBox();
      const badge = document.createElementNS(ns, 'g');
      badge.setAttribute('class', `badge badge-${s}`);
      const t = document.createElementNS(ns, 'text');
      t.textContent = BADGE[s];
      badge.append(t);
      g.append(badge);
      const w = t.getComputedTextLength() + 12, h = 16;
      const x = box.x + box.width - w + 6, y = box.y - h / 2 - 1;
      const pill = document.createElementNS(ns, 'rect');
      Object.entries({ x, y, width: w, height: h, rx: 4 }).forEach(([k, v]) => pill.setAttribute(k, v));
      t.setAttribute('x', x + 6);
      t.setAttribute('y', y + h / 2 + 0.5);
      badge.prepend(pill);
    }
    const otherKeys = new Set(otherEdges.values());
    let edges = 0;
    for (const [id, key] of myEdges) {
      if (otherKeys.has(key)) continue;
      edges++;
      for (const g of svg.querySelectorAll(`[data-id="${CSS.escape(id)}"]`)) if (!g.classList.contains('pele-node')) g.dataset.st = gone;
    }
    return { st, edges };
  }
  const mb = mark(sb, bb, ba, eb, ea, 'removed');
  const ma = mark(sa, ba, bb, ea, eb, 'added');

  function finish(svg, marked) {
    svg.removeAttribute('width');
    svg.removeAttribute('height');
    const b = svg.getBBox();
    const p = 14;
    const vb = { x: b.x - p, y: b.y - p, w: Math.ceil(b.width + p * 2), h: Math.ceil(b.height + p * 2) };
    svg.setAttribute('viewBox', `${vb.x} ${vb.y} ${vb.w} ${vb.h}`);
    svg.setAttribute('width', vb.w);
    svg.setAttribute('height', vb.h);
    svg.style.maxWidth = 'none';
    const origin = svg.getBoundingClientRect();
    const out = {};
    for (const g of svg.querySelectorAll('.pele-node[data-id]')) {
      const id = g.dataset.id;
      if (out[id]) continue;
      // The block's own shape, without its badge.
      const shapes = [...g.children].filter((c) => !c.classList.contains('badge'));
      const r = shapes.map((c) => c.getBoundingClientRect()).filter((r) => r.width > 0 && r.height > 0);
      if (!r.length) continue;
      const x0 = Math.min(...r.map((q) => q.left)), y0 = Math.min(...r.map((q) => q.top));
      const x1 = Math.max(...r.map((q) => q.right)), y1 = Math.max(...r.map((q) => q.bottom));
      out[id] = { x: x0 - origin.left, y: y0 - origin.top, w: x1 - x0, h: y1 - y0, status: marked.st.get(id) ?? 'same', label: text(g).replace(/\|/g, ' ') };
    }
    const count = (s) => [...marked.st.values()].filter((v) => v === s).length;
    return { w: vb.w, h: vb.h, stats: { added: count('added'), removed: count('removed'), changed: count('changed'), edges: marked.edges }, blocks: out };
  }
  return { before: finish(sb, mb), after: finish(sa, ma) };
}

async function main() {
  const spec = JSON.parse(readFileSync(join(dir, 'changes.json'), 'utf8'));
  if (!spec.before?.trim() || !spec.after?.trim()) fail('в changes.json нужны обе диаграммы: before и after');

  const chromium = loadChromium();
  const pele = await peleBundle();
  const browser = await chromium.launch().catch(() => chromium.launch({ channel: 'chrome' }));
  try {
    const page = await browser.newPage({ deviceScaleFactor: 2, viewport: { width: 2400, height: 1600 } });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.setContent(`<!doctype html><meta charset="utf-8"><style>${fontFaces()}${CSS}</style><body></body>`);
    await page.addScriptTag({ path: pele });
    await page.evaluate(() => document.fonts.load('14px "Instrument Sans"'));
    const result = await page.evaluate(renderBoth, {
    before: spec.before,
    after: spec.after,
    // A block the agent says changed counts as changed even when its label did not.
    changed: Object.entries(spec.blocks ?? {}).filter(([, b]) => b?.change?.trim()).map(([id]) => id),
  });
    if (result.error) fail(result.error);
    for (const theme of ['light', 'dark']) {
      for (const plain of [false, true]) {
        await page.evaluate(({ theme, plain }) => {
          document.body.className = `${theme}${plain ? '' : ' diff'}`;
        }, { theme, plain });
        for (const side of ['before', 'after']) {
          const name = `${side}-${theme}${plain ? '-plain' : ''}.png`;
          await page.locator(`#${side} svg`).screenshot({ path: join(dir, name), omitBackground: true });
        }
      }
    }
    if (errors.length) fail(errors.join('\n'));
    writeFileSync(join(dir, 'layout.json'), JSON.stringify(result, null, 1));
    console.log('ok');
  } finally {
    await browser.close();
  }
}

await main();
