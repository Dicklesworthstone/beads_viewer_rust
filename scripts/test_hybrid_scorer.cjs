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
    console: { log() {}, warn(...args) { warnings.push(args); } },
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
