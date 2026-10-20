'use strict';

// Run the entire shipped charts asset. Chart.js is a recording boundary here:
// these tests inspect data passed to it, not Chart.js drawing or a CLI export.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const { spawnSync } = require('node:child_process');

const ASSET = path.join(process.env.BVR_CHART_ASSET_DIR || path.join(__dirname, '..', 'viewer_assets'), 'charts.js');
const SOURCE = fs.readFileSync(ASSET, 'utf8');
const NOW = Date.parse('2026-10-20T12:00:00Z');
const DAY = 86400000;
const plain = value => JSON.parse(JSON.stringify(value));

function runtime(now = NOW) {
  class FixedDate extends Date {
    constructor(...args) { super(...(args.length ? args : [now])); }
    static now() { return now; }
  }
  const charts = [];
  class Chart {
    static defaults = { font: {} };
    constructor(canvas, config) {
      this.data = config.data;
      this.options = config.options;
      this.canvas = canvas;
      this.updates = 0;
      this.destroyed = false;
      charts.push(this);
    }
    update() { this.updates += 1; }
    destroy() { this.destroyed = true; }
  }
  const context = vm.createContext({ Date: FixedDate, Chart, console: { log() {} } });
  context.window = context;
  context.document = {
    getElementById(id) { return id.endsWith('-chart') ? { getContext() { return id; } } : null; },
  };
  vm.runInContext(SOURCE, context, { filename: ASSET });
  return {
    context, charts,
    compute(issues) {
      context.fixture = issues;
      return plain(vm.runInContext('chartsState.issues = fixture; computeBurndownData()', context, { timeout: 1500 }));
    },
  };
}

function issue(id, created_at, extras = {}) {
  return { id, title: id, status: 'open', priority: 2, issue_type: 'task', created_at, ...extras };
}
function latest(data) { return [data.open.at(-1), data.closed.at(-1), data.blocked.at(-1)]; }
function assertSeries(data) {
  assert.ok(data.labels.length <= 31, 'allocate only the displayed samples');
  for (const key of ['open', 'closed', 'blocked']) {
    assert.equal(data[key].length, data.labels.length);
    assert.ok(data[key].every(value => Number.isSafeInteger(value) && value >= 0));
  }
}

test('empty, missing, invalid and future-only creation dates produce no invented history', () => {
  const { compute } = runtime();
  for (const rows of [[], [issue('MISSING')], [issue('BAD', 'not-a-date')],
    [issue('FUTURE', '2026-10-21T00:00:00Z')], [issue('EMPTY', '')]]) {
    assert.deepEqual(compute(rows), { labels: [], open: [], closed: [], blocked: [] });
  }
});

test('today includes creations and closures before now but excludes future creations', () => {
  const { compute } = runtime();
  const data = compute([
    issue('OPEN', '2026-10-20T09:00:00Z'),
    issue('CLOSED', '2026-10-20T10:00:00Z', { status: 'closed', closed_at: '2026-10-20T11:00:00Z' }),
    issue('BLOCKED', '2026-10-20T10:30:00Z', { status: 'blocked' }),
    issue('LATER', '2026-10-20T16:00:00Z'),
  ]);
  assert.deepEqual(data.labels, ['Oct 20']);
  assert.deepEqual(latest(data), [1, 1, 1]);
  assertSeries(data);
});

test('UTC day-end samples include intraday creations and closures without a one-day lag', () => {
  const { compute } = runtime();
  const data = compute([
    issue('ROOT', '2026-10-18T23:00:00Z'),
    issue('DONE', '2026-10-18T10:00:00Z', { status: 'closed', closed_at: '2026-10-19T11:00:00Z' }),
    issue('WAIT', '2026-10-19T22:00:00Z', { status: 'blocked' }),
  ]);
  assert.deepEqual(data.labels, ['Oct 18', 'Oct 19', 'Oct 20']);
  assert.deepEqual(data.open, [2, 1, 1]);
  assert.deepEqual(data.closed, [0, 1, 1]);
  assert.deepEqual(data.blocked, [0, 1, 1]);
});

test('creation clock time cannot omit the final day of the series', () => {
  const data = runtime().compute([issue('LATE', '2026-10-18T23:59:59Z')]);
  assert.equal(data.labels.at(-1), 'Oct 20');
  assert.deepEqual(latest(data), [1, 0, 0]);
});

test('midnight boundaries assign events to their actual UTC day', () => {
  const data = runtime().compute([
    issue('OLD', '2026-10-18T00:00:00Z', { status: 'closed', closed_at: '2026-10-19T00:00:00Z' }),
    issue('NEW', '2026-10-19T00:00:00Z'),
  ]);
  assert.deepEqual(data.open, [1, 1, 1]);
  assert.deepEqual(data.closed, [0, 1, 1]);
});

test('camelCase timestamps and offset-bearing dates use the same absolute timeline', () => {
  const { compute } = runtime();
  const snake = [issue('A', '2026-10-18T23:00:00-04:00', {
    status: 'closed', closed_at: '2026-10-20T10:00:00+02:00',
  })];
  const camel = [{ id: 'A', status: 'closed', createdAt: '2026-10-19T03:00:00Z', closedAt: '2026-10-20T08:00:00Z' }];
  assert.deepEqual(compute(camel), compute(snake));
  assert.deepEqual(compute(snake).closed, [0, 1]);
});

test('reopened issues ignore stale close timestamps instead of remaining closed forever', () => {
  const data = runtime().compute([
    issue('REOPENED', '2026-10-18T00:00:00Z', { closed_at: '2026-10-19T00:00:00Z' }),
    issue('REBLOCKED', '2026-10-18T00:00:00Z', { status: 'blocked', closed_at: '2026-10-19T00:00:00Z' }),
  ]);
  assert.deepEqual(data.open, [1, 1, 1]);
  assert.deepEqual(data.blocked, [1, 1, 1]);
  assert.deepEqual(data.closed, [0, 0, 0]);
});

test('closed issues without a usable close date use updated time then the observation time', () => {
  const data = runtime().compute([
    issue('UPDATED', '2026-10-18T10:00:00Z', { status: 'closed', updated_at: '2026-10-19T11:00:00Z' }),
    issue('UNKNOWN', '2026-10-18T10:00:00Z', { status: 'closed' }),
    issue('INVALID', '2026-10-18T10:00:00Z', { status: 'closed', closed_at: 'invalid', updated_at: 'invalid' }),
    issue('FUTURE', '2026-10-18T10:00:00Z', { status: 'closed', closed_at: '9999-01-01T00:00:00Z' }),
  ]);
  assert.deepEqual(data.open, [4, 3, 0]);
  assert.deepEqual(data.closed, [0, 1, 4]);
});

test('completion before creation cannot introduce a negative or phantom issue count', () => {
  const data = runtime().compute([
    issue('ANCHOR', '2026-10-18T00:00:00Z'),
    issue('DONE', '2026-10-20T09:00:00Z', { status: 'closed', closed_at: '2026-10-17T00:00:00Z' }),
  ]);
  assert.deepEqual(data.open, [1, 1, 1]);
  assert.deepEqual(data.closed, [0, 0, 1]);
  assertSeries(data);
});

test('normalized closed statuses and update aliases agree with current snapshot totals', () => {
  const data = runtime().compute([
    issue('CLOSED', '2026-10-19T09:00:00Z', { status: ' CLOSED ', updatedAt: '2026-10-20T11:00:00Z' }),
    issue('DELETED', '2026-10-19T09:00:00Z', { status: 'tombstone', updated_at: '2026-10-20T11:00:00Z' }),
    issue('BLOCKED', '2026-10-19T09:00:00Z', { status: ' BLOCKED ' }),
    issue('RUNNING', '2026-10-19T09:00:00Z', { status: 'in_progress' }),
  ]);
  assert.deepEqual(data.open, [3, 1]);
  assert.deepEqual(data.closed, [0, 2]);
  assert.deepEqual(data.blocked, [1, 1]);
});

test('explicit closure outranks a later update and closure exactly now is included', () => {
  const data = runtime().compute([
    issue('DONE', '2026-10-18T09:00:00Z', {
      status: 'closed', closed_at: '2026-10-19T09:00:00Z', updated_at: '2026-10-20T10:00:00Z',
    }),
    issue('JUST-DONE', '2026-10-18T09:00:00Z', { status: 'closed', closed_at: new Date(NOW).toISOString() }),
  ]);
  assert.deepEqual(data.closed, [0, 1, 2]);
  assert.deepEqual(data.open, [2, 1, 0]);
});

test('long ranges have at most 31 samples and retain both endpoint totals', () => {
  const data = runtime().compute([
    issue('FIRST', '2026-01-01T20:00:00Z'),
    issue('LAST', '2026-10-20T11:59:59Z'),
  ]);
  assertSeries(data);
  assert.equal(data.labels.length, 31);
  assert.equal(data.labels[0], 'Jan 1');
  assert.equal(data.labels.at(-1), 'Oct 20');
  assert.equal(data.open[0], 1);
  assert.equal(data.open.at(-1), 2);
});

test('an ancient valid date cannot multiply work by every intervening day', () => {
  const rows = Array.from({ length: 10000 }, (_, i) => issue(`I-${i}`, '0001-01-01T00:00:00Z'));
  const data = runtime().compute(rows);
  assertSeries(data);
  assert.equal(data.labels.length, 31);
  assert.equal(data.open[0], rows.length);
  assert.equal(data.open.at(-1), rows.length);
});

test('150000 creation dates do not exceed the JavaScript argument-count limit', () => {
  const rows = Array.from({ length: 150000 }, (_, i) => issue(`I-${i}`, '2026-10-20T01:00:00Z'));
  const data = runtime().compute(rows);
  assert.deepEqual(latest(data), [rows.length, 0, 0]);
  assertSeries(data);
});

test('ordinary midnight snapshots retain the established open, blocked and closed totals', () => {
  const data = runtime().compute([
    issue('A', '2026-10-18T00:00:00Z'),
    issue('B', '2026-10-18T00:00:00Z', { status: 'closed', closed_at: '2026-10-19T00:00:00Z' }),
    issue('C', '2026-10-19T00:00:00Z', { status: 'blocked' }),
  ]);
  assert.deepEqual(data.open, [2, 1, 1]);
  assert.deepEqual(data.closed, [0, 1, 1]);
  assert.deepEqual(data.blocked, [0, 1, 1]);
});

test('source records remain unchanged across repeated timeline computations', () => {
  const rows = [issue('A', '2026-10-18T09:00:00Z', { status: 'closed', closed_at: '2026-10-19T10:00:00Z' })];
  const before = structuredClone(rows);
  const { compute } = runtime();
  const first = compute(rows);
  assert.deepEqual(compute(rows), first);
  assert.deepEqual(rows, before);
});

test('public init and update pass the corrected data to Chart.js and preserve cleanup', () => {
  const { context, charts } = runtime();
  context.bvCharts.init([issue('A', '2026-10-20T09:00:00Z')], []);
  assert.equal(charts.length, 3);
  assert.deepEqual(plain(charts[0].data.datasets[0].data), [1]);
  context.bvCharts.update([issue('A', '2026-10-20T09:00:00Z', {
    status: 'closed', closed_at: '2026-10-20T11:00:00Z',
  })], []);
  assert.deepEqual(plain(charts[0].data.datasets[0].data), [0]);
  assert.deepEqual(plain(charts[0].data.datasets[1].data), [1]);
  assert.equal(charts[0].updates, 1);
  context.bvCharts.destroy();
  assert.ok(charts.every(chart => chart.destroyed));
});

test('day labels stay UTC across host timezones and DST boundaries', () => {
  const probe = `
    const fs = require('node:fs'), vm = require('node:vm');
    const context = { window: {} }; vm.createContext(context);
    vm.runInContext(fs.readFileSync(process.argv[1], 'utf8'), context);
    console.log(JSON.stringify(['2026-03-08','2026-11-01','2026-10-20'].map(context.formatDateLabel)));
  `;
  for (const TZ of ['UTC', 'America/New_York', 'Pacific/Honolulu', 'Pacific/Auckland']) {
    const output = spawnSync(process.execPath, ['-e', probe, ASSET], {
      env: { ...process.env, TZ }, encoding: 'utf8', timeout: 3000,
    });
    assert.equal(output.status, 0, output.stderr);
    assert.deepEqual(JSON.parse(output.stdout), ['Mar 8', 'Nov 1', 'Oct 20'], TZ);
  }
});

// A simple direct counting oracle is intentionally different from the production
// sampled-delta algorithm. Small deterministic fixtures compare every daily point.
test('seeded short-range snapshots agree with an independent day-end counting oracle', () => {
  let seed = 7141;
  const random = () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed; };
  for (let trial = 0; trial < 20; trial += 1) {
    const start = Date.parse('2026-10-14T00:00:00Z');
    const rows = [issue('ANCHOR', new Date(start).toISOString())];
    for (let i = 0; i < 80; i += 1) {
      const created = start + random() % (NOW - start + 1);
      const status = ['open', 'blocked', 'closed'][random() % 3];
      const closed = created + random() % (NOW - created + 1);
      rows.push(issue(`I-${i}`, new Date(created).toISOString(), {
        status, ...(status === 'closed' ? { closed_at: new Date(closed).toISOString() } : {}),
      }));
    }
    const expected = { open: [], closed: [], blocked: [] };
    for (let day = start; day <= NOW; day += DAY) {
      const cutoff = Math.min(day + DAY - 1, NOW);
      const counts = { open: 0, closed: 0, blocked: 0 };
      for (const value of rows) {
        if (Date.parse(value.created_at) > cutoff) continue;
        if (value.status === 'closed' && Date.parse(value.closed_at) <= cutoff) counts.closed += 1;
        else if (value.status === 'blocked') counts.blocked += 1;
        else counts.open += 1;
      }
      for (const key of Object.keys(expected)) expected[key].push(counts[key]);
    }
    const data = runtime().compute(rows);
    for (const key of Object.keys(expected)) assert.deepEqual(data[key], expected[key], `trial ${trial}: ${key}`);
  }
});
