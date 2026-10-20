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
