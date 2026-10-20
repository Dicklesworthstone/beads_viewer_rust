'use strict';

// Runs the shipped asset scripts unchanged in V8; no browser or npm dependency.
// BVR_HYBRID_ASSET_DIR can point at a pinned parent checkout for negative controls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');

const ASSETS = process.env.BVR_HYBRID_ASSET_DIR
  || path.join(__dirname, '..', 'viewer_assets');
const NOW = Date.parse('2026-01-15T12:00:00Z');

function runtime({ fallback = true } = {}) {
  class FixedDate extends Date {
    static now() { return NOW; }
  }
  const warnings = [];
  const context = vm.createContext({
    Date: FixedDate,
    console: { log() {}, error() {}, warn(...args) { warnings.push(args); } },
  });
  context.window = context;
  for (const name of fallback
    ? ['hybrid_scorer.js', 'wasm_loader.js']
    : ['wasm_loader.js']) {
    vm.runInContext(fs.readFileSync(path.join(ASSETS, name), 'utf8'), context, {
      filename: name,
    });
  }
  return { context, warnings };
}

function row(id, overrides = {}) {
  return {
    id, title: `Issue ${id}`, textScore: 0.8, pagerank: 0.25,
    status: 'open', priority: 1, blockerCount: 5,
    updatedAt: '2025-12-16T12:00:00Z', ...overrides,
  };
}

function ids(rows) {
  return Array.from(rows, value => value.id);
}

function finiteRanking(rows) {
  for (const value of rows) {
    assert.ok(Number.isFinite(value.hybrid_score), `invalid score for ${value.id}`);
    assert.ok(value.hybrid_score >= 0 && value.hybrid_score <= 1);
    for (const [name, component] of Object.entries(value.component_scores)) {
      assert.ok(Number.isFinite(component), `${value.id}: invalid ${name}`);
      assert.ok(component >= 0 && component <= 1, `${value.id}: unbounded ${name}`);
    }
  }
}

function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

test('ordinary preset scoring preserves weighted components and input records', () => {
  const { context } = runtime();
  const rows = [row('A'), row('B', {
    textScore: 0.2, pagerank: 0.1, status: 'closed', priority: 4,
    blockerCount: 10, updatedAt: null,
  })];
  const before = structuredClone(rows);
  const ranked = new context.HybridScorer().scoreAndRank(rows);
  assert.deepEqual(ids(ranked), ['A', 'B']);
  const expected = 0.40 * 0.8 + 0.20 * 0.25 + 0.15 * 1
    + 0.10 * 0.5 + 0.10 * 0.8 + 0.05 * Math.exp(-1);
  assert.ok(Math.abs(ranked[0].hybrid_score - expected) < 1e-12);
  assert.equal(ranked[0].component_scores.recency, Math.exp(-1));
  assert.equal(ranked[1].component_scores.recency, 0.5);
  assert.deepEqual(rows, before);
  assert.notEqual(ranked[0], rows[0]);
  finiteRanking(ranked);
});

for (const updatedAt of ['2026-01-16T12:00:00Z', '9999-01-01T00:00:00Z']) {
  test(`future timestamp is bounded: ${updatedAt}`, () => {
    const { context } = runtime();
    const ranked = new context.HybridScorer().scoreAndRank([row('FUTURE', { updatedAt })]);
    finiteRanking(ranked);
    assert.equal(ranked[0].component_scores.recency, 1);
  });
}

test('text-only search cannot be poisoned by an overflowing future recency', () => {
  const { context } = runtime();
  const ranked = new context.HybridScorer(context.HYBRID_PRESETS['text-only']).scoreAndRank([
    row('WEAK', { textScore: 0.1, updatedAt: '9999-01-01T00:00:00Z' }),
    row('BEST', { textScore: 0.9 }),
  ]);
  assert.deepEqual(ids(ranked), ['BEST', 'WEAK']);
  assert.equal(ranked[0].hybrid_score, 0.9);
  assert.equal(ranked[1].hybrid_score, 0.1);
  finiteRanking(ranked);
});

test('missing and invalid timestamps use the neutral recency score', () => {
  const { context } = runtime();
  for (const updatedAt of [undefined, null, '', 'not-a-date']) {
    const ranked = new context.HybridScorer().scoreAndRank([row('A', { updatedAt })]);
    finiteRanking(ranked);
    assert.equal(ranked[0].component_scores.recency, 0.5);
  }
});

test('very old timestamps remain valid and cannot cause overflow', () => {
  const { context } = runtime();
  const ranked = new context.HybridScorer().scoreAndRank([
    row('OLD', { updatedAt: '0001-01-01T00:00:00Z' }),
  ]);
  assert.equal(ranked[0].component_scores.recency, 0);
  finiteRanking(ranked);
});

test('unknown statuses do not inherit Object prototype values', () => {
  const { context } = runtime();
  for (const status of ['review', 'constructor', '__proto__', 'toString']) {
    const ranked = new context.HybridScorer().scoreAndRank([row(status, { status })]);
    finiteRanking(ranked);
    assert.equal(ranked[0].component_scores.status, 0.5);
  }
});

test('non-finite text and graph scores use finite defaults', () => {
  const { context } = runtime();
  for (const bad of [undefined, null, NaN, Infinity, -Infinity]) {
    const ranked = new context.HybridScorer().scoreAndRank([
      row('A', { textScore: bad, pagerank: bad }),
    ]);
    finiteRanking(ranked);
    assert.equal(ranked[0].component_scores.pagerank, 0.5);
  }
});

test('out-of-range normalized inputs cannot dominate the score', () => {
  const { context } = runtime();
  for (const bad of [-10, 10, Number.MAX_VALUE]) {
    finiteRanking(new context.HybridScorer().scoreAndRank([
      row('A', { textScore: bad, pagerank: bad }),
    ]));
  }
});

test('invalid impact counts do not contaminate the batch maximum', () => {
  const { context } = runtime();
  const ranked = new context.HybridScorer().scoreAndRank([
    row('BAD', { blockerCount: Infinity }),
    row('REAL', { blockerCount: 5 }),
    row('NONE', { blockerCount: -2 }),
  ]);
  finiteRanking(ranked);
  assert.equal(ranked.find(value => value.id === 'REAL').component_scores.impact, 1);
  assert.equal(ranked.find(value => value.id === 'BAD').component_scores.impact, 0);
  assert.equal(ranked.find(value => value.id === 'NONE').component_scores.impact, 0);
});

test('equal scores have deterministic exact-ID ordering across input permutations', () => {
  const { context } = runtime();
  const rows = [row('Z'), row('A'), row('a'), row('A,1')];
  for (let i = 0; i < rows.length; i += 1) {
    const ranked = new context.HybridScorer().scoreAndRank(rows);
    assert.deepEqual(ids(ranked), ['A', 'A,1', 'Z', 'a']);
    rows.push(rows.shift());
  }
});

test('empty input and reused scorer retain their existing behavior', () => {
  const { context } = runtime();
  const scorer = new context.HybridScorer();
  assert.deepEqual(ids(scorer.scoreAndRank([])), []);
  scorer.scoreAndRank([row('A', { blockerCount: 100 })]);
  const ranked = scorer.scoreAndRank([row('B', { blockerCount: 0 })]);
  assert.equal(ranked[0].component_scores.impact, 0.5);
  finiteRanking(ranked);
});

test('missing WASM uses the actual JavaScript scorer and preserves metadata', () => {
  const { context } = runtime();
  const rows = [row('Z'), row('A', { description: '<b>literal source</b>' })];
  const expected = new context.HybridScorer().scoreAndRank(rows);
  const actual = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(plain(actual), plain(expected));
  assert.equal(actual.find(value => value.id === 'A').description, '<b>literal source</b>');
});

// Fault injection replaces only the optional WASM constructor. Batch matching,
// validation, fallback scoring and cleanup execute the shipped loader verbatim.
function installBackend(context, reply, { constructError, scoreError, freeError } = {}) {
  const calls = { constructed: 0, scored: 0, freed: 0, payload: null };
  context.TestBackend = class {
    constructor() {
      calls.constructed += 1;
      if (constructError) throw new Error('constructor failed');
    }
    score_batch(raw) {
      calls.scored += 1;
      calls.payload = JSON.parse(raw);
      if (scoreError) throw new Error('scoring failed');
      const result = typeof reply === 'function' ? reply(calls.payload) : reply;
      return typeof result === 'string' ? result : JSON.stringify(result);
    }
    free() {
      calls.freed += 1;
      if (freeError) throw new Error('cleanup failed');
    }
  };
  vm.runInContext('HybridWasmCtor = TestBackend', context);
  return calls;
}

function scored(id, score = 0.9) {
  return { id, score, components: [0.2, 1, 0.5, 0.8, 0.5] };
}

const invalidBatches = [
  ['duplicate IDs with unchanged count', [scored('A'), scored('A')]],
  ['unknown ID replacing a result', [scored('A'), scored('UNKNOWN')]],
  ['unknown extra result', [scored('B'), scored('UNKNOWN'), scored('A', 0.1)]],
  ['missing result', [scored('B')]],
  ['empty batch', []],
  ['object instead of batch', { results: [scored('B'), scored('A')] }],
  ['null batch', null],
  ['null entry', [null, scored('B')]],
  ['empty ID', [scored(''), scored('B')]],
  ['numeric ID', [scored(1), scored('B')]],
  ['missing score', [{ id: 'A', components: [0, 0, 0, 0, 0] }, scored('B')]],
  ['null score', [scored('A', null), scored('B')]],
  ['string score', [scored('A', '0.99'), scored('B')]],
  ['boolean score', [scored('A', true), scored('B')]],
  ['non-finite score', '[{"id":"A","score":1e999,"components":[0,0,0,0,0]},{"id":"B","score":0.9,"components":[0,0,0,0,0]}]'],
  ['missing components', [{ id: 'A', score: 0.1 }, scored('B')]],
  ['short components', [{ ...scored('A', 0.1), components: [0.5] }, scored('B')]],
  ['extra components', [{ ...scored('A', 0.1), components: [0, 0, 0, 0, 0, 0] }, scored('B')]],
  ['null component', [{ ...scored('A', 0.1), components: [0, 0, null, 0, 0] }, scored('B')]],
  ['string component', [{ ...scored('A', 0.1), components: [0, 0, '0.5', 0, 0] }, scored('B')]],
  ['non-finite component', '[{"id":"A","score":0.1,"components":[0,0,1e999,0,0]},{"id":"B","score":0.9,"components":[0,0,0,0,0]}]'],
  ['invalid JSON', '{unfinished'],
];

for (const [name, reply] of invalidBatches) {
  test(`WASM ${name} falls back to the complete JavaScript ranking`, () => {
    const { context } = runtime();
    const rows = [row('A', { textScore: 0.9 }), row('B', { textScore: 0.1 })];
    const before = structuredClone(rows);
    const expected = new context.HybridScorer().scoreAndRank(rows);
    const calls = installBackend(context, reply);
    const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
    finiteRanking(ranked);
    assert.deepEqual(plain(ranked), plain(expected));
    assert.deepEqual(rows, before);
    assert.equal(calls.constructed, 1);
    assert.equal(calls.scored, 1);
    assert.equal(calls.freed, 1);
  });
}

test('valid WASM scores are used and preserve exact IDs and original metadata', () => {
  const { context } = runtime();
  const rows = [
    row('A,1', { description: '<img src=x onerror="sentinel()">' }),
    row('__proto__', { textScore: 0.1 }),
  ];
  const before = structuredClone(rows);
  const calls = installBackend(context, [scored('A,1', 0.1), scored('__proto__', 0.9)]);
  const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(ids(ranked), ['__proto__', 'A,1']);
  assert.equal(ranked[0].hybrid_score, 0.9, 'must not silently disable acceleration');
  assert.equal(ranked[1].description, rows[0].description);
  assert.deepEqual(rows, before);
  assert.equal(calls.freed, 1);
  finiteRanking(ranked);
});

test('equal WASM scores use the same exact-ID tie order as JavaScript', () => {
  const { context } = runtime();
  const rows = [row('Z'), row('A')];
  const calls = installBackend(context, [scored('Z', 0.5), scored('A', 0.5)]);
  assert.deepEqual(ids(context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default)), ['A', 'Z']);
  assert.equal(calls.freed, 1);
});

test('ambiguous input IDs retain both original rows instead of duplicating the last', () => {
  const { context } = runtime();
  const rows = [row('A', { title: 'First', textScore: 0.9 }), row('A', { title: 'Second', textScore: 0.1 })];
  const expected = new context.HybridScorer().scoreAndRank(rows);
  const calls = installBackend(context, [scored('A'), scored('A')]);
  const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(plain(ranked), plain(expected));
  assert.equal(calls.constructed, 0, 'ambiguous identities cannot be merged safely');
});

test('empty input IDs use a lossless fallback', () => {
  const { context } = runtime();
  const rows = [row(''), row('B')];
  const expected = new context.HybridScorer().scoreAndRank(rows);
  const calls = installBackend(context, [scored(''), scored('B')]);
  assert.deepEqual(plain(context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default)), plain(expected));
  assert.equal(calls.constructed, 0);
});

test('constructor failure uses JavaScript without attempting to free an absent instance', () => {
  const { context } = runtime();
  const rows = [row('A'), row('B')];
  const calls = installBackend(context, null, { constructError: true });
  assert.deepEqual(
    plain(context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default)),
    plain(new context.HybridScorer().scoreAndRank(rows)),
  );
  assert.equal(calls.scored, 0);
  assert.equal(calls.freed, 0);
});

test('cleanup failure cannot replace a valid accelerated ranking with an exception', () => {
  const { context, warnings } = runtime();
  const rows = [row('A'), row('B')];
  const calls = installBackend(context, [scored('B', 0.9), scored('A', 0.1)], { freeError: true });
  const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(ids(ranked), ['B', 'A']);
  assert.equal(ranked[0].hybrid_score, 0.9);
  assert.equal(calls.freed, 1);
  assert.equal(warnings.length, 1, 'cleanup failure is still observable');
});

test('simultaneous score and cleanup failures preserve the complete fallback', () => {
  const { context } = runtime();
  const rows = [row('A'), row('B')];
  const calls = installBackend(context, null, { scoreError: true, freeError: true });
  const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(plain(ranked), plain(new context.HybridScorer().scoreAndRank(rows)));
  assert.equal(calls.freed, 1);
});

test('valid acceleration works without the optional JavaScript fallback library', () => {
  const { context } = runtime({ fallback: false });
  const calls = installBackend(context, [scored('A', 0.1), scored('B', 0.9)]);
  const rows = [row('A'), row('B')];
  const ranked = context.scoreBatchHybrid(rows, { text: 1 });
  assert.deepEqual(ids(ranked), ['B', 'A']);
  assert.equal(ranked[0].hybrid_score, 0.9);
  assert.equal(calls.freed, 1);
});

test('malformed acceleration without JavaScript returns every original result', () => {
  const { context } = runtime({ fallback: false });
  installBackend(context, [scored('A'), scored('A')]);
  const rows = [row('A'), row('B')];
  const ranked = context.scoreBatchHybrid(rows, { text: 1 });
  assert.equal(ranked, rows, 'neither backend may fabricate a partial replacement');
});

test('future dates are sent to WASM as zero age, not the missing-date sentinel', () => {
  const { context } = runtime();
  const rows = [row('FUTURE', { updatedAt: '9999-01-01T00:00:00Z' }), row('MISSING', { updatedAt: null })];
  const calls = installBackend(context, [scored('FUTURE'), scored('MISSING', 0.1)]);
  context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.equal(calls.payload[0].days_since_update, 0);
  assert.equal(calls.payload[1].days_since_update, -1);
});

test('large accelerated batches retain all 5001 identities exactly once', () => {
  const { context } = runtime();
  const rows = Array.from({ length: 5001 }, (_, index) => row(`id-${String(index).padStart(5, '0')}`));
  const calls = installBackend(context, payload => payload.map(value => scored(value.id, 0.5)).reverse());
  const ranked = context.scoreBatchHybrid(rows, context.HYBRID_PRESETS.default);
  assert.deepEqual(ids(ranked), ids(rows));
  assert.equal(new Set(ids(ranked)).size, rows.length);
  assert.equal(calls.freed, 1);
  finiteRanking(ranked);
});

// Load the complete viewer script, with only browser event registration stubbed.
// Its real filter builder and query/count wrappers execute against a sql.js-shaped
// adapter backed by node:sqlite FTS5. This does not run Alpine, the actual sql.js
// WASM runtime, or the CLI exporter. The existing viewer CI uses Node 24.
function searchRuntime({ fallback = true, execute } = {}) {
  const { context, warnings } = runtime({ fallback });
  const source = fs.readFileSync(path.join(ASSETS, 'viewer.js'), 'utf8');
  context.addEventListener = () => {};
  context.document = { addEventListener() {} };
  vm.runInContext(source, context, { filename: 'viewer.js' });
  const query = execute || (() => { throw new Error('query adapter not supplied'); });
  context.beadsViewer.DB_STATE.db = {
    exec(sql, params) {
      const rows = query(sql, params);
      if (!rows.length) return [];
      const columns = Object.keys(rows[0]);
      return [{ columns, values: rows.map(value => columns.map(column => value[column])) }];
    },
  };
  context.queryIssues = () => { throw new Error('unexpected FTS fallback'); };
  return { context, warnings };
}

function searchDatabase(t) {
  const { DatabaseSync } = require('node:sqlite');
  const db = new DatabaseSync(':memory:');
  t.after(() => db.close());
  db.exec(`
    CREATE TABLE issue_overview_mv (
      id TEXT PRIMARY KEY, title TEXT, description TEXT, labels TEXT,
      assignee TEXT, status TEXT, priority INTEGER, pagerank REAL,
      blocker_count INTEGER, updated_at TEXT
    );
    CREATE VIRTUAL TABLE issues_fts USING fts5(
      id UNINDEXED, title, description, labels, assignee
    );
  `);
  const insert = db.prepare('INSERT INTO issue_overview_mv VALUES (?,?,?,?,?,?,?,?,?,?)');
  for (const [id, title, description] of [
    ['BEST', 'nebula nebula nebula', 'nebula observing guide'],
    ['MEDIUM', 'nebula', 'observing handbook with background notes'],
    ['WEAK', 'Journal', `nebula ${'unrelated lengthy background '.repeat(80)}`],
    ['OTHER', 'Different topic', 'no query term here'],
  ]) {
    insert.run(id, title, description, '[]', '', 'open', 2, 0.5, 0, '2026-01-15T12:00:00Z');
  }
  db.exec('INSERT INTO issues_fts SELECT id,title,description,labels,assignee FROM issue_overview_mv');
  const execute = (sql, params) => db.prepare(sql).all(...params);
  // Independent SQL ordering establishes the expected relevance direction.
  const expected = db.prepare(`
    SELECT id, bm25(issues_fts, 3.0, 2.0, 1.0, 1.5, 0.5) AS score
    FROM issues_fts WHERE issues_fts MATCH ? ORDER BY score, id
  `).all('nebula*');
  assert.deepEqual(ids(expected), ['BEST', 'MEDIUM', 'WEAK']);
  assert.ok(expected.every(value => value.score < 0));
  assert.ok(expected[0].score < expected[1].score && expected[1].score < expected[2].score);
  return { db, execute, expected };
}

for (const preset of ['text-only', 'default', 'bug-hunting', 'sprint-planning', 'impact-first']) {
  test(`Pages ${preset} preserves actual SQLite FTS5 relevance`, t => {
    const { execute, expected } = searchDatabase(t);
    const { context } = searchRuntime({ execute });
    const ranked = context.searchIssues('nebula', { mode: 'hybrid', preset });
    assert.deepEqual(ids(ranked), ids(expected));
    assert.equal(ranked[0].text_score, 1);
    assert.ok(ranked[0].text_score > ranked[1].text_score);
    assert.ok(ranked[1].text_score > ranked[2].text_score);
    for (const result of ranked) {
      assert.equal(result.bm25_score, expected.find(value => value.id === result.id).score);
    }
    assert.ok(ranked[0].snippet.includes('<mark>nebula</mark>'));
    finiteRanking(ranked);
  });
}

test('Pages relevance is corrected before either accelerated or fallback ranking', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  const calls = installBackend(context, payload => payload.map(value => scored(value.id, value.text_score)).reverse());
  const ranked = context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' });
  assert.deepEqual(ids(ranked), ids(expected));
  assert.equal(calls.payload.find(value => value.id === 'BEST').text_score, 1);
  assert.ok(calls.payload.find(value => value.id === 'WEAK').text_score < 1);
  assert.equal(calls.freed, 1);
});

test('Pages uses the correct SQL relevance when the optional scorer wrapper is absent', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  context.scoreBatchHybrid = undefined;
  assert.deepEqual(ids(context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' })), ids(expected));
});

test('Pages accelerator failure does not restore the reversed SQL relevance', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  const calls = installBackend(context, null, { scoreError: true, freeError: true });
  assert.deepEqual(ids(context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' })), ids(expected));
  assert.equal(calls.freed, 1);
});

test('Pages hybrid pagination slices the correctly ranked candidate pool', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  const pages = expected.map((_, offset) => context.searchIssues('nebula', {
    mode: 'hybrid', preset: 'text-only', limit: 1, offset,
  }));
  assert.deepEqual(pages.flatMap(ids), ids(expected));
  assert.deepEqual(ids(context.searchIssues('nebula', {
    mode: 'hybrid', preset: 'text-only', limit: 1, offset: expected.length,
  })), []);
});

test('Pages missing or invalid BM25 scores never outrank genuine stronger matches', () => {
  const invalid = [undefined, null, 0, -0, NaN, Infinity, -Infinity, 'not-a-score', '-100', 5];
  const rows = [row('STRONG', { bm25_score: -4 }), row('WEAK', { bm25_score: -1 }),
    ...invalid.map((bm25_score, index) => row(`BAD-${index}`, { bm25_score }))];
  const before = structuredClone(rows);
  const { context } = searchRuntime({ execute: () => rows });
  const ranked = context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' });
  assert.deepEqual(ids(ranked).slice(0, 2), ['STRONG', 'WEAK']);
  assert.equal(ranked[0].text_score, 1);
  assert.equal(ranked[1].text_score, 0.25);
  assert.ok(ranked.slice(2).every(value => value.text_score === 0));
  assert.deepEqual(rows, before, 'source hits and metadata remain unchanged');
  finiteRanking(ranked);
});

test('Pages handles the full finite negative BM25 range without overflow', () => {
  const rows = [row('STRONG', { bm25_score: -Number.MAX_VALUE }),
    row('WEAK', { bm25_score: -Number.MIN_VALUE })];
  const { context } = searchRuntime({ execute: () => rows });
  const ranked = context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' });
  assert.deepEqual(ids(ranked), ['STRONG', 'WEAK']);
  assert.equal(ranked[0].text_score, 1);
  assert.equal(ranked[1].text_score, 0);
  finiteRanking(ranked);
});

test('Pages normalizes large candidate sets without a spread argument limit', () => {
  const rows = Array.from({ length: 150000 }, (_, index) => ({
    id: String(index).padStart(6, '0'), bm25_score: -(index + 1),
    status: 'open', priority: 2, pagerank: 0.5,
  }));
  const { context } = searchRuntime({ execute: () => rows });
  const ranked = context.searchIssues('nebula', {
    mode: 'hybrid', preset: 'text-only', limit: rows.length,
  });
  assert.equal(ranked.length, rows.length);
  assert.equal(ranked[0].id, rows.at(-1).id);
  assert.equal(ranked.at(-1).id, rows[0].id);
  assert.equal(ranked[0].text_score, 1);
  assert.equal(new Set(ids(ranked)).size, rows.length);
});

test('Pages zero-information BM25 batches retain neutral text contribution', () => {
  const { context } = searchRuntime({ execute: () => [row('Z', { bm25_score: 0 }), row('A')] });
  const ranked = context.searchIssues('nebula', { mode: 'hybrid', preset: 'text-only' });
  assert.deepEqual(ids(ranked), ['A', 'Z']);
  assert.ok(ranked.every(value => value.text_score === 0.5));
  finiteRanking(ranked);
});

test('Pages text mode retains direct SQLite ordering and pagination', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  assert.deepEqual(ids(context.searchIssues('nebula')), ids(expected));
  assert.deepEqual(ids(context.searchIssues('nebula', { offset: 1, limit: 1 })), ['MEDIUM']);
});

test('Pages missing hybrid library retains the SQLite ranked rows', t => {
  const { execute, expected } = searchDatabase(t);
  const { context } = searchRuntime({ execute, fallback: false });
  assert.deepEqual(ids(context.searchIssues('nebula', { mode: 'hybrid' })), ids(expected));
});

test('Pages exact-ID promotion still precedes relevance order', () => {
  // This fixture isolates promotion; hyphens in raw FTS expressions are a
  // separate query-parsing concern, so do not claim it tests the SQL parser.
  const { context } = searchRuntime({ execute: () => [
    row('OTHER', { bm25_score: -10 }), row('BD-1', { bm25_score: -1 }),
  ] });
  assert.deepEqual(ids(context.searchIssues('bd-1', { mode: 'hybrid', preset: 'text-only' })), ['BD-1', 'OTHER']);
});

test('Pages empty FTS results remain empty', t => {
  const { execute } = searchDatabase(t);
  const { context } = searchRuntime({ execute });
  assert.deepEqual(ids(context.searchIssues('absentterm', { mode: 'hybrid' })), []);
});

test('Pages FTS errors preserve the existing query fallback arguments', () => {
  const { context } = searchRuntime({ execute: () => { throw new Error('no FTS'); } });
  const fallbackRows = [row('FALLBACK')];
  let captured;
  context.queryIssues = (...args) => { captured = plain(args); return fallbackRows; };
  assert.equal(context.searchIssues('nebula', {
    mode: 'hybrid', limit: 2, offset: 3, filters: { status: 'open' },
  }), fallbackRows);
  assert.deepEqual(captured, [{ status: 'open', search: 'nebula' }, 'score', 2, 3]);
});

for (const available of ['neither', 'scorer-only', 'presets-only']) {
  test(`Pages hybrid pagination falls back to SQL when ${available} is available`, t => {
    const { execute, expected } = searchDatabase(t);
    const queries = [];
    const { context } = searchRuntime({
      fallback: false,
      execute: (sql, params) => {
        queries.push(plain(params));
        return execute(sql, params);
      },
    });
    if (available === 'scorer-only') {
      vm.runInContext('class HybridScorer { constructor() { throw new Error("not usable without presets"); } }', context);
    } else if (available === 'presets-only') {
      vm.runInContext('const HYBRID_PRESETS = { default: { text: 1 } };', context);
    }

    const pages = [];
    for (let offset = 0; offset <= expected.length; offset += 1) {
      pages.push(context.searchIssues('nebula', { mode: 'hybrid', limit: 1, offset }));
    }
    assert.deepEqual(pages.flatMap(ids), ids(expected), 'every match appears once across pages');
    assert.deepEqual(ids(pages.at(-1)), [], 'past the final page must be empty');
    assert.deepEqual(queries.map(params => params.slice(-2)), [[1, 0], [1, 1], [1, 2], [1, 3]],
      'SQL performs the requested pagination rather than fetching the first candidate pool repeatedly');
  });
}

test('Pages missing hybrid libraries preserve active filter parameters and page offsets', t => {
  const { db, execute } = searchDatabase(t);
  db.prepare("UPDATE issue_overview_mv SET status = 'closed' WHERE id = ?").run('MEDIUM');
  const queries = [];
  const { context } = searchRuntime({
    fallback: false,
    execute: (sql, params) => { queries.push(plain(params)); return execute(sql, params); },
  });
  const filters = { status: 'open', search: 'ignored stale search' };
  const pages = [0, 1, 2].map(offset => context.searchIssues('nebula', {
    mode: 'hybrid', limit: 1, offset, filters,
  }));
  assert.deepEqual(pages.flatMap(ids), ['BEST', 'WEAK']);
  assert.deepEqual(ids(pages.at(-1)), []);
  assert.deepEqual(queries, [['nebula*', 'open', 1, 0], ['nebula*', 'open', 1, 1], ['nebula*', 'open', 1, 2]]);
  assert.equal(filters.search, 'ignored stale search', 'caller filters remain unchanged');
});

test('Pages equal-relevance SQL pages use exact issue ID ordering across insertion orders', t => {
  const { db, execute } = searchDatabase(t);
  const permutations = [
    ['Z', 'a', 'A,1', 'A'],
    ['A,1', 'A', 'a', 'Z'],
  ];
  for (const order of permutations) {
    db.exec('DELETE FROM issues_fts; DELETE FROM issue_overview_mv;');
    const insert = db.prepare('INSERT INTO issue_overview_mv VALUES (?,?,?,?,?,?,?,?,?,?)');
    for (const id of order) {
      insert.run(id, 'nebula', 'same content', '[]', '', 'open', 2, 0.5, 0, null);
    }
    db.exec('INSERT INTO issues_fts SELECT id,title,description,labels,assignee FROM issue_overview_mv ORDER BY rowid');
    for (const [mode, fallback] of [['text', true], ['hybrid', false]]) {
      const { context } = searchRuntime({ execute, fallback });
      const pages = order.map((_, offset) => context.searchIssues('nebula', { mode, limit: 1, offset }));
      assert.deepEqual(pages.flatMap(ids), ['A', 'A,1', 'Z', 'a'], `${mode}: insertion order cannot determine page boundaries`);
    }
  }
});

for (const fallback of [true, false]) {
  test(`Pages application next/previous navigation with hybrid library ${fallback ? 'present' : 'absent'}`, t => {
    const { execute } = searchDatabase(t);
    const { context } = searchRuntime({ execute, fallback });
    context.localStorage = { getItem: () => null };
    context.location = { hash: '#/issues?q=nebula' };
    context.URLSearchParams = URLSearchParams;
    context.history = {
      replaceState(_state, _title, hash) { context.location.hash = hash; },
    };
    const app = context.beadsApp();
    app.view = 'issues';
    app.searchQuery = 'nebula';
    app.searchMode = 'hybrid';
    app.searchPreset = 'text-only';
    app.pageSize = 1;
    app.loadIssues();
    assert.equal(app.totalIssues, 3, 'count and list query use the same database');
    assert.deepEqual(ids(app.issues), ['BEST']);
    app.nextPage();
    assert.equal(app.page, 2);
    assert.deepEqual(ids(app.issues), ['MEDIUM']);
    app.nextPage();
    assert.equal(app.page, 3);
    assert.deepEqual(ids(app.issues), ['WEAK']);
    app.nextPage();
    assert.equal(app.page, 3, 'the final page is retained');
    app.prevPage();
    assert.deepEqual(ids(app.issues), ['MEDIUM']);
    app.prevPage();
    app.prevPage();
    assert.equal(app.page, 1, 'navigation cannot go before page one');
    assert.deepEqual(ids(app.issues), ['BEST']);
    assert.equal(context.location.hash, '#/issues?q=nebula');
  });
}
