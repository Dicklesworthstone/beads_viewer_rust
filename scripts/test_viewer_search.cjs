#!/usr/bin/env node
'use strict';

// Run with an existing Playwright installation (no npm project is needed):
// NODE_PATH=/path/to/node_modules node --test scripts/test_viewer_search.cjs
// BVR_CHROMIUM_EXECUTABLE (or CHROMIUM_PATH) selects an installed Chromium.
// BVR_VIEWER_REVISION optionally serves index.html/viewer.js/graph.js from a git revision
// to demonstrate that these regressions fail before a fix, without changing files.

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { readFile } = require('node:fs/promises');
const http = require('node:http');
const path = require('node:path');
const { after, before, test } = require('node:test');
const { chromium } = require('playwright');

const root = path.resolve(__dirname, '..');
const assets = path.join(root, 'viewer_assets');
const specialQuery = 'C++ / #28 & café 50%?';
const issues = [
  ['bv-open', 'Needle open ready', '', 'open', 1, 'bug', 'Ada', '["viewer"]', '', 2],
  ['bv-blocked', 'Needle open blocked', '', 'open', 2, 'task', 'Ben', '["frontend"]', 'bv-open', 0],
  ['bv-progress', 'Needle in progress', '', 'in_progress', 0, 'feature', 'Ada', '["backend"]', '', 0],
  ['bv-closed', 'Needle closed', '', 'closed', 3, 'bug', '', '["viewer"]', '', 0],
  ['bv-description', 'Description match', 'A needle appears in the description.', 'open', 4, 'task', 'Ben', '["frontend"]', '', 0],
  ['bv-other', 'Unrelated blocked task', '', 'open', 2, 'task', '', '[]', 'bv-open', 0],
  ['bv-special', specialQuery, '', 'open', 2, 'task', '', '[]', '', 0],
];
const dependencies = [
  ['bv-blocked', 'bv-open', 'blocks'],
  ['bv-other', 'bv-open', 'blocks'],
];
// A separate graph fixture keeps the original search expectations unchanged.
// Closing root makes left/right ready; completing BOTH then unlocks join/tail.
const graphIssues = [
  ['graph-root', 'Root prerequisite', '', 'open', 1, 'task', '', '[]', '', 2],
  ['graph-left', 'Left branch', '', 'open', 2, 'task', '', '[]', 'graph-root', 1],
  ['graph-right', 'Right branch', '', 'open', 2, 'task', '', '[]', 'graph-root', 1],
  ['graph-join', 'Two prerequisite join', '', 'open', 2, 'task', '', '[]', 'graph-left,graph-right', 1],
  ['graph-tail', 'Tail after join', '', 'open', 2, 'task', '', '[]', 'graph-join', 0],
  ['graph-closed', 'Completed prerequisite', '', 'closed', 2, 'task', '', '[]', '', 0],
  ['graph-tombstone', 'Tombstoned prerequisite', '', 'tombstone', 2, 'task', '', '[]', '', 0],
  ['graph-ready', 'Ready after resolved prerequisites', '', 'open', 2, 'task', '', '[]', '', 0],
  ['graph-isolated', 'Standalone actionable issue', '', 'open', 2, 'task', '', '[]', '', 0],
  ['graph-info', 'Nonblocking relationships', '', 'open', 2, 'task', '', '[]', '', 0],
  ['graph-external', 'Reference outside this export', '', 'open', 2, 'task', '', '[]', '', 0],
];
const graphDependencies = [
  ['graph-left', 'graph-root', 'blocks'],
  ['graph-right', 'graph-root', 'waits-for'],
  ['graph-join', 'graph-left', 'conditional-blocks'],
  ['graph-join', 'graph-right', 'blocks'],
  ['graph-tail', 'graph-join', 'blocks'],
  ['graph-ready', 'graph-closed', 'blocks'],
  ['graph-ready', 'graph-tombstone', 'waits-for'],
  ['graph-info', 'graph-root', 'related'],
  ['graph-info', 'graph-tail', 'parent-child'],
  ['graph-external', 'missing-blocker', 'blocks'],
  ['missing-dependent', 'graph-root', 'blocks'],
];
// Match data/history.json emitted by export_pages.rs: a flat map keyed by ID,
// with individually timestamped events and empty commit SHAs. Same-day entries
// must remain separate; the current database includes a closed prerequisite.
const historyEvent = (id, kind, timestamp, message = '', sha = '') => ({
  bead_id: id, event_type: kind, timestamp, commit_sha: sha,
  commit_message: message, author: '', author_email: '',
});
const recordedEvents = [
  historyEvent('graph-root', 'created', '2026-10-06T09:00:00Z', 'Root recorded'),
  historyEvent('graph-closed', 'created', '2026-10-06T09:00:00Z', 'Same instant, separate issue'),
  historyEvent('graph-left', 'created', '2026-10-06T09:00:30Z', 'Left recorded'),
  historyEvent('graph-root', 'updated', '2026-10-06T09:01:00Z', '<b>Literal recorded update</b>'),
  historyEvent('graph-closed', 'closed', '2026-10-06T09:02:00Z', 'Completed prerequisite'),
];
function exportedHistory(events) {
  const histories = {};
  for (const event of events) {
    const issue = graphIssues.find(item => item[0] === event.bead_id);
    const history = histories[event.bead_id] ||= {
      bead_id: event.bead_id, title: issue?.[1] || event.bead_id,
      status: issue?.[3] || 'open', events: [], milestones: {},
      commits: null, cycle_time: null, last_author: '',
    };
    history.events.push(event);
  }
  return histories;
}
const recordedHistory = exportedHistory([
  ...recordedEvents,
  historyEvent('graph-root', 'updated', '', 'No invented date'),
  historyEvent('graph-left', 'updated', 'not-a-date', 'Invalid date'),
  historyEvent('graph-left', 'updated', '2026', 'A year is not an event timestamp'),
  historyEvent('graph-left', 'updated', 1791277200000, 'Timestamp strings are required'),
  historyEvent('graph-left', 'updated', '2026-02-30T09:00:00Z', 'Invalid calendar day'),
  historyEvent('not-in-export', 'created', '2026-10-06T09:03:00Z', 'Unknown issue'),
]);
const allIds = issues.map(issue => issue[0]);
const matchingIds = ['bv-open', 'bv-blocked', 'bv-progress', 'bv-closed', 'bv-description'];
const openIds = ['bv-open', 'bv-blocked', 'bv-description'];
const rowSelector = '[role="button"][aria-label^="View issue "]';
const inputSelector = 'input[placeholder="Search issues..."]:visible';
const executablePath = process.env.BVR_CHROMIUM_EXECUTABLE || process.env.CHROMIUM_PATH;
let server;
let origin;

async function fixtureDatabase(fixtureIssues = issues, fixtureDependencies = dependencies) {
  const initSqlJs = require(path.join(assets, 'vendor/sql-wasm.js'));
  const SQL = await initSqlJs({ locateFile: file => path.join(assets, 'vendor', file) });
  const db = new SQL.Database();
  // Reuse the real export schema, rather than maintain a second column contract.
  const exporter = await readFile(path.join(root, 'src/export_sqlite.rs'), 'utf8');
  for (const table of ['issue_overview_mv', 'dependencies', 'export_meta']) {
    const schema = exporter.match(new RegExp(`CREATE TABLE IF NOT EXISTS ${table} \\([\\s\\S]*?\\);`));
    assert.ok(schema, `export schema contains ${table}`);
    db.run(schema[0]);
  }
  db.run('CREATE VIEW issues AS SELECT * FROM issue_overview_mv');
  db.run("INSERT INTO export_meta VALUES ('issue_count', ?)", [String(fixtureIssues.length)]);
  for (const issue of fixtureIssues) {
    db.run(`INSERT INTO issue_overview_mv
      (id, title, description, status, priority, issue_type, assignee, labels,
       blocked_by_ids, blocks_count, triage_score, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0.5, '2026-10-06T12:00:00Z', '2026-10-06T12:00:00Z')`, issue);
  }
  for (const dependency of fixtureDependencies) {
    db.run('INSERT INTO dependencies (issue_id, depends_on_id, type) VALUES (?, ?, ?)', dependency);
  }
  // Deliberately omit issues_fts: the shipped LIKE fallback must work without FTS5,
  // as it does in the original report. No application/search/router is mocked.
  const bytes = Buffer.from(db.export());
  db.close();
  return bytes;
}

before(async () => {
  const database = await fixtureDatabase();
  const graphDatabase = await fixtureDatabase(graphIssues, graphDependencies);
  const overrides = new Map();
  if (process.env.BVR_VIEWER_REVISION) {
    for (const file of ['index.html', 'viewer.js', 'graph.js']) {
      overrides.set(file, execFileSync('git', ['show', `${process.env.BVR_VIEWER_REVISION}:viewer_assets/${file}`], { cwd: root }));
    }
  }
  const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm' };
  server = http.createServer(async (request, response) => {
    const requestedPath = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const graphFixture = requestedPath.startsWith('/graph-fixture/');
    const pathname = graphFixture ? requestedPath.slice('/graph-fixture'.length) : requestedPath;
    response.setHeader('Cache-Control', 'no-store');
    // Supply the isolation headers directly, as a configured preview server can,
    // so COI service-worker installation does not reload a test mid-interaction.
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'credentialless');
    if (pathname === '/beads.sqlite3') {
      response.writeHead(200, { 'Content-Type': 'application/octet-stream' });
      response.end(graphFixture ? graphDatabase : database);
      return;
    }
    if (pathname === '/beads.sqlite3.config.json') {
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.end('{}');
      return;
    }
    if (graphFixture && pathname === '/data/history.json') {
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify(recordedHistory));
      return;
    }
    const relative = pathname === '/' ? 'index.html' : pathname.slice(1);
    const filename = path.resolve(assets, relative);
    if (!filename.startsWith(assets + path.sep)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const bytes = overrides.get(relative) || await readFile(filename);
      response.writeHead(200, { 'Content-Type': types[path.extname(filename)] || 'application/octet-stream' });
      response.end(bytes);
    } catch (error) {
      response.writeHead(error.code === 'ENOENT' ? 404 : 500).end();
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  origin = `http://127.0.0.1:${server.address().port}`;
});

after(async () => {
  if (server) {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
});

async function searchInput(page, mobile) {
  if (mobile && !await page.locator(inputSelector).count()) {
    await page.locator('header button:visible').filter({ has: page.locator('svg path[d^="M21 21l-6-6"]') }).click();
  }
  return page.locator(inputSelector);
}

async function openViewer(t, mobile, hash = '#/', useClock = false, fixturePath = '', setupPage) {
  // Keep browser process state and memory independent between scenarios.
  const browser = await chromium.launch({
    headless: true,
    // Retain the system-browser flags used by this suite's original runner.
    args: executablePath ? [
      '--no-sandbox', '--disable-dev-shm-usage', '--no-zygote',
      '--use-gl=angle', '--use-angle=swiftshader',
    ] : [],
    ...(executablePath ? { executablePath } : {}),
  });
  let context;
  const errors = [];
  const consoleErrors = [];
  t.after(async () => {
    try {
      if (context) await context.close();
      if (errors.length) t.diagnostic(JSON.stringify({ errors, consoleErrors }, null, 2));
      assert.deepEqual(errors, [], 'no uncaught browser errors');
    } finally {
      await browser.close();
    }
  });
  context = await browser.newContext({
    viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 },
    isMobile: mobile,
    hasTouch: mobile,
    reducedMotion: 'reduce',
  });
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  page.setDefaultNavigationTimeout(30000);
  if (useClock) await page.clock.install({ time: new Date('2026-10-07T12:00:00Z') });
  if (hash === '#/graph') {
    // Keep the real WASM request in flight beyond the graph view's 50ms staging
    // delay, so both initial-route consumers encounter the same initialization.
    await page.route('**/vendor/bv_graph_bg.wasm', async request => {
      await new Promise(resolve => setTimeout(resolve, 200));
      await request.continue();
    });
  }
  page.on('pageerror', error => errors.push({ name: error.name, message: error.message, stack: error.stack }));
  page.on('console', message => {
    if (message.type() === 'error') {
      consoleErrors.push({ text: message.text(), location: message.location() });
      if (consoleErrors.length > 10) consoleErrors.shift();
    }
  });
  if (process.env.VIEWER_TEST_DEBUG) page.on('console', message => console.log(message.text()));
  // Preserve non-Error rejection values (Alpine can reject a plain object) in
  // failure diagnostics. Observe only: do not suppress browser error reporting.
  await page.addInitScript(() => {
    window.addEventListener('unhandledrejection', event => {
      let reason;
      try { reason = JSON.stringify(event.reason); }
      catch { reason = String(event.reason); }
      console.error('[viewer test] unhandled rejection:', reason);
    });
  });
  if (setupPage) await setupPage(page);
  await page.goto(origin + '/' + fixturePath + hash);
  await page.locator('[x-show="loading"]').waitFor({ state: 'hidden', timeout: 30000 });
  await searchInput(page, mobile);
  return page;
}

function listView(page) {
  return page.locator('[x-show="view === \'issues\'"]');
}

function route(page) {
  const hash = new URL(page.url()).hash;
  return { path: hash.split('?')[0], params: new URLSearchParams(hash.split('?')[1] || '') };
}

async function results(page, ids, query) {
  await listView(page).waitFor({ state: 'visible' });
  await page.waitForFunction(({ selector, expected }) => {
    const actual = Array.from(document.querySelectorAll(selector))
      .filter(row => row.getClientRects().length)
      .map(row => row.getAttribute('aria-label').match(/^View issue ([^:]+):/)[1])
      .sort();
    return JSON.stringify(actual) === JSON.stringify(expected);
  }, { selector: rowSelector, expected: [...ids].sort() });
  assert.equal(await page.locator(inputSelector).inputValue(), query);
  assert.equal(route(page).path, '#/issues');
  assert.equal(route(page).params.get('q'), query || null);
  assert.equal(await listView(page).locator('[x-text="totalIssues"]').first().textContent(), String(ids.length));
}

async function home(page, mobile, advanceClock = false) {
  const link = page.getByRole('link', { name: mobile ? 'Home' : 'Dashboard', exact: true });
  if (advanceClock) await navigateWithClock(page, () => link.click());
  else await link.click();
  await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
}

async function navigateWithClock(page, action) {
  // A native hash change is a browser task, not a fake timer. Wait until its
  // listeners have run before advancing Alpine's deferred rendering callbacks.
  const changed = page.evaluate(() => new Promise(resolve => {
    window.addEventListener('hashchange', () => resolve(), { once: true });
  }));
  await Promise.all([changed, action()]);
  await page.clock.runFor(50);
}

async function filters(page, mobile) {
  if (mobile) await listView(page).getByRole('button', { name: /^Filters/ }).click();
}

async function insightsImpact(page, issueId, directCount, cascadeIds) {
  await page.locator('[x-show="view === \'insights\'"]').waitFor({ state: 'visible' });
  const picks = page.locator('[x-show="topKSet && topKSet.items?.length > 0"]');
  await picks.waitFor({ state: 'visible' });
  const firstPick = picks.locator('.group').first();
  assert.equal(await firstPick.locator('[x-text="item.issueId"]').textContent(), issueId,
    'suggested work starts with the prerequisite that unlocks its dependents');
  assert.equal(await picks.locator('[x-text^="topKSet"]').textContent(), String(cascadeIds.length));
  assert.match(await firstPick.locator('span.rounded-full').innerText(), new RegExp(`\\+${cascadeIds.length} unblock`));
  assert.deepEqual((await firstPick.locator('[x-text="unblockedId"]').allTextContents()).sort(), [...cascadeIds].sort());
  const impact = page.locator('[x-show="topImpactIssues?.length > 0"]').locator('.group').first();
  assert.equal(await impact.locator('[x-text="item.issueId"]').textContent(), issueId);
  assert.match(await impact.locator('span.shrink-0').innerText(), new RegExp(`^${cascadeIds.length}\\s+potential unblocks$`));

  await firstPick.click();
  const modal = page.locator('[x-show="selectedIssue"]');
  await modal.waitFor({ state: 'visible' });
  assert.equal(route(page).path, `#/issue/${issueId}`);
  await modal.getByRole('button', { name: 'Simulate Close', exact: true }).click();
  const result = modal.locator('[x-show="whatIfResult"]');
  await result.waitFor({ state: 'visible' });
  assert.deepEqual(await result.locator('p .font-bold').allTextContents(), [String(directCount), String(cascadeIds.length)]);
  assert.deepEqual((await result.locator('button').allTextContents()).sort(), [...cascadeIds].sort(),
    'the rendered cascade lists the affected issues');
  await modal.locator('button').first().click();
  await modal.waitFor({ state: 'hidden' });
}

async function openForceGraph(page) {
  if (route(page).path !== '#/graph') {
    await page.getByRole('link', { name: 'Graph', exact: true }).click();
  }
  await page.waitForFunction(() => {
    const app = Alpine.$data(document.querySelector('[x-data="beadsApp()"]'));
    return app.forceGraphReady && !app.forceGraphLoading && app.forceGraphModule?.getWasmGraph()?.nodeCount() > 0;
  });
  await page.locator('#graph-container canvas').first().waitFor({ state: 'visible' });
}

async function dependencyPath(page, issueId) {
  return page.evaluate(async id => {
    const graph = await import('./graph.js');
    const node = graph.getGraph().graphData().nodes.find(item => item.id === id);
    let result;
    document.addEventListener('bv-graph:pathHighlight', event => {
      result = { blockers: event.detail.blockerCount, dependents: event.detail.dependentCount };
    }, { once: true });
    graph.highlightDependencyPath(node);
    return result;
  }, issueId);
}

async function forceGraphImpact(page, issueId) {
  // Invoke the same public action used by Shift-click / W, then observe the
  // actual animation, summary event and toast. No graph algorithm is replaced.
  return page.evaluate(async id => {
    const graph = await import('./graph.js');
    const node = graph.getGraph().graphData().nodes.find(item => item.id === id);
    const completed = new Promise(resolve => {
      document.addEventListener('bv-graph:whatIfComplete', event => resolve({
        direct: event.detail.directUnblocks,
        total: event.detail.transitiveUnblocks,
        directIds: [...event.detail.unblockedIds].sort(),
        ids: [...event.detail.cascadeIds].sort(),
      }), { once: true });
    });
    const result = graph.performWhatIf(node);
    if (!result) throw new Error(`what-if did not run for ${id}`);
    const summary = await completed;
    return {
      direct: result.direct_unblocks,
      total: result.transitive_unblocks,
      ids: result.cascade_ids.map(index => graph.getWasmGraph().nodeId(index)).sort(),
      summary,
      animated: graph.getGraph().graphData().nodes
        .filter(item => item._whatIfState === 'unblocked').map(item => item.id).sort(),
    };
  }, issueId);
}

async function currentGraphSnapshot(page) {
  return page.evaluate(async () => {
    const graph = (await import('./graph.js')).getGraph().graphData();
    const endpoint = node => typeof node === 'object' ? node.id : node;
    return {
      nodes: graph.nodes.map(node => ({ id: node.id, status: node.status, title: node.title }))
        .sort((a, b) => a.id.localeCompare(b.id)),
      links: graph.links.map(link => [endpoint(link.source), endpoint(link.target), link.type || ''])
        .sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))),
    };
  });
}

async function historyEventVisible(page, event, index, total, snapshot) {
  const controls = page.locator('#time-travel-controls');
  await controls.waitFor({ state: 'visible' });
  assert.equal(await controls.locator('#tt-position').textContent(), `${index + 1} / ${total} events`);
  assert.equal(await controls.locator('#tt-issue').textContent(), event.bead_id);
  assert.equal((await controls.locator('#tt-kind').textContent()).toLowerCase(), event.event_type);
  assert.equal(await controls.locator('#tt-date').getAttribute('datetime'), event.timestamp);
  assert.ok((await controls.locator('#tt-date').textContent()).includes(event.timestamp.slice(11, 19)),
    'recorded seconds remain visible for same-day events');
  assert.equal(await controls.locator('#tt-message').textContent(), event.commit_message);
  assert.equal(await controls.locator('#tt-message b').count(), 0, 'event details are literal text');
  assert.equal(await controls.getByRole('slider', { name: 'History event', exact: true }).inputValue(), String(index));

  const observed = await page.evaluate(async () => {
    const module = await import('./graph.js');
    const state = module.getTimeTravelState();
    const graph = module.getGraph();
    const draw = graph.nodeCanvasObject();
    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d');
    let fills = [];
    const fill = ctx.fill;
    // Observe the actual renderer on a real Canvas2D context. Calling the
    // original fill preserves rendering; no selection/graph algorithm is mocked.
    ctx.fill = function (...args) {
      fills.push({ color: this.fillStyle, alpha: this.globalAlpha });
      return fill.apply(this, args);
    };
    const rendered = graph.graphData().nodes.map(node => {
      fills = [];
      draw(node, ctx, 1);
      const body = fills.filter(item => typeof item.color === 'string').at(-1);
      return { id: node.id, alpha: body?.alpha };
    });
    const brightest = Math.max(...rendered.map(node => node.alpha));
    return {
      active: state.active, currentIdx: state.currentIdx, totalEvents: state.totalEvents,
      issue: state.currentEvent?.issueId, highlighted: state.highlightedIssueIds,
      renderedHighlight: rendered.filter(node => node.alpha === brightest).map(node => node.id),
    };
  });
  assert.deepEqual(observed, {
    active: true, currentIdx: index, totalEvents: total,
    issue: event.bead_id, highlighted: [event.bead_id], renderedHighlight: [event.bead_id],
  }, 'the selected recorded event highlights its actual canvas node');
  assert.deepEqual(await currentGraphSnapshot(page), snapshot,
    'recorded events never invent or remove historical nodes, statuses, or links');
}

async function selectHistoryEvent(page, index) {
  await page.getByRole('slider', { name: 'History event', exact: true }).evaluate((slider, value) => {
    slider.value = String(value);
    slider.dispatchEvent(new Event('input', { bubbles: true }));
  }, index);
}

for (const mobile of [false, true]) {
  const device = mobile ? 'mobile' : 'desktop';

  test(`${device}: dashboard search shows matching Issues and survives navigation/history`, async t => {
    const page = await openViewer(t, mobile);
    await page.locator(inputSelector).fill('needle');
    await results(page, matchingIds, 'needle');
    await page.getByRole('link', { name: 'Issues', exact: true }).click();
    await results(page, matchingIds, 'needle');
    if (mobile) {
      await home(page, mobile);
      await page.locator('header button:visible').filter({ has: page.locator('svg path[d="M4 6h16M4 12h16M4 18h16"]') }).click();
      const menuLink = page.locator('header').getByRole('link', { name: 'Issues', exact: true });
      assert.equal(new URLSearchParams((await menuLink.getAttribute('href')).split('?')[1]).get('q'), 'needle');
      await menuLink.click();
      await results(page, matchingIds, 'needle');
    }
    await home(page, mobile);
    const href = await page.getByRole('link', { name: 'Issues', exact: true }).getAttribute('href');
    assert.equal(new URLSearchParams(href.split('?')[1]).get('q'), 'needle');
    await page.getByRole('link', { name: 'Issues', exact: true }).click();
    await results(page, matchingIds, 'needle');
    await page.goBack();
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    await page.goBack();
    await results(page, matchingIds, 'needle');
    await page.goForward();
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    await page.goForward();
    await results(page, matchingIds, 'needle');
  });

  test(`${device}: restored routes replace absent filters, query and sort`, async t => {
    const page = await openViewer(t, mobile, '#/issues?q=needle');
    await results(page, matchingIds, 'needle');
    const dense = '#/issues?status=open&type=bug&priority=1&labels=viewer&assignee=Ada&blocked=false&blocking=true&q=needle&sort=title';
    await page.goto(origin + '/' + dense);
    await results(page, ['bv-open'], 'needle');
    await page.goBack();
    await results(page, matchingIds, 'needle');
    assert.deepEqual([...route(page).params.keys()], ['q']);
    assert.equal(await listView(page).locator('select[x-model="sort"]').inputValue(), 'priority');
    await page.goForward();
    await results(page, ['bv-open'], 'needle');
    await page.goto(origin + '/#/issues');
    await results(page, allIds, '');
    assert.equal(route(page).params.size, 0);
    // Let the clear button's finite leave animation finish before ordinary
    // history navigation reverses it; debounce races are tested separately.
    await page.locator('button[title="Clear search"]').nth(mobile ? 1 : 0).waitFor({ state: 'hidden' });
    await page.goBack();
    await results(page, ['bv-open'], 'needle');
  });

  test(`${device}: issue details close to the same search and filter`, async t => {
    const page = await openViewer(t, mobile, '#/issues?status=open&q=needle');
    await results(page, openIds, 'needle');
    await page.getByRole('button', { name: 'View issue bv-open: Needle open ready', exact: true }).click();
    const modal = page.locator('[x-show="selectedIssue"]');
    await modal.waitFor({ state: 'visible' });
    assert.equal(route(page).path, '#/issue/bv-open');
    await modal.locator('button').first().click();
    await modal.waitFor({ state: 'hidden' });
    await results(page, openIds, 'needle');
    assert.equal(route(page).params.get('status'), 'open');
    await page.goBack();
    await modal.waitFor({ state: 'visible' });
    await page.goBack();
    await modal.waitFor({ state: 'hidden' });
    await results(page, openIds, 'needle');
    if (!mobile) {
      // The keyboard also opens a modal without changing the Issues route.
      await page.getByRole('link', { name: 'Issues', exact: true }).focus();
      await page.keyboard.press('o');
      await modal.waitFor({ state: 'visible' });
      assert.equal(route(page).path, '#/issues');
      await page.keyboard.press('/');
      await page.keyboard.insertText('needle');
      await modal.waitFor({ state: 'hidden' });
      await results(page, openIds, 'needle');
    }
  });

  test(`${device}: status/blocked changes preserve search and clear controls retain their meaning`, async t => {
    const page = await openViewer(t, mobile, '#/issues?q=needle');
    await results(page, matchingIds, 'needle');
    const historyLength = await page.evaluate(() => history.length);
    await page.locator('select[x-model="searchMode"]:visible').selectOption('hybrid');
    await page.locator('select[x-model="searchPreset"]:visible').selectOption('bug-hunting');
    await results(page, matchingIds, 'needle');
    await filters(page, mobile);
    await listView(page).getByRole('button', { name: 'open', exact: true }).click();
    await results(page, openIds, 'needle');
    await listView(page).getByRole('button', { name: 'Blocked', exact: true }).click();
    await results(page, ['bv-blocked'], 'needle');
    await page.locator('button[title="Clear search"]:visible').click();
    await results(page, ['bv-blocked', 'bv-other'], '');
    assert.equal(route(page).params.get('status'), 'open');
    assert.equal(route(page).params.get('blocked'), 'true');
    await listView(page).getByRole('button', { name: 'Clear all', exact: true }).click();
    await results(page, allIds, '');
    assert.equal(route(page).params.size, 0);
    assert.equal(await page.evaluate(() => history.length), historyLength, 'in-place filters and searches do not add history entries');
  });

  test(`${device}: dashboard status and blocked cards retain query and create history entries`, async t => {
    const page = await openViewer(t, mobile, '#/issues?q=needle');
    const cards = () => page.locator('[x-show="view === \'dashboard\'"]')
      .locator(mobile ? '.stat-card-mobile' : '.card-lift.cursor-pointer');
    await home(page, mobile);
    await cards().filter({ has: page.getByText('Open', { exact: true }) }).click();
    await results(page, openIds, 'needle');
    await page.goBack();
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    assert.equal(route(page).path, '#/');
    await page.goForward();
    await results(page, openIds, 'needle');
    await home(page, mobile);
    await cards().filter({ has: page.getByText('Blocked', { exact: true }) }).click();
    await results(page, ['bv-blocked'], 'needle');
    assert.equal(route(page).params.get('blocked'), 'true');
    await page.goBack();
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    await page.goForward();
    await results(page, ['bv-blocked'], 'needle');
  });

  test(`${device}: quick navigation commits input on Issues and cancels a pending search elsewhere`, async t => {
    const page = await openViewer(t, mobile, '#/', true);
    await page.clock.pauseAt(new Date('2026-10-07T13:00:00Z'));
    await page.locator(inputSelector).fill('needle');
    await page.clock.runFor(1);
    assert.equal(route(page).path, '#/', 'the input debounce is still pending');
    await navigateWithClock(page, () => page.getByRole('link', { name: 'Issues', exact: true }).click());
    await results(page, matchingIds, 'needle');
    await home(page, mobile, true);
    await page.locator(inputSelector).fill('unrelated');
    await page.clock.runFor(1);
    await navigateWithClock(page, () => page.getByRole('link', { name: 'Insights', exact: true }).click());
    await page.locator('[x-show="view === \'insights\'"]').waitFor({ state: 'visible' });
    // Cross the 300ms debounce deterministically; it must not pull us back.
    await page.clock.fastForward(450);
    assert.equal(route(page).path, '#/insights');
    assert.equal(await page.locator(inputSelector).inputValue(), 'unrelated');
    await navigateWithClock(page, () => page.getByRole('link', { name: 'Issues', exact: true }).click());
    await results(page, ['bv-other'], 'unrelated');
  });

  test(`${device}: Back cancels pending input and empty dashboard searches stay put`, async t => {
    const page = await openViewer(t, mobile, '#/', true);
    await page.locator(inputSelector).fill('needle');
    await results(page, matchingIds, 'needle');
    await page.clock.pauseAt(new Date('2026-10-07T13:00:00Z'));
    await page.locator(inputSelector).fill('unrelated');
    await page.clock.runFor(1);
    assert.equal(route(page).params.get('q'), 'needle', 'replacement input has not committed yet');
    await navigateWithClock(page, () => page.goBack());
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    await page.clock.fastForward(450);
    assert.equal(route(page).path, '#/');
    assert.equal(await page.locator(inputSelector).inputValue(), 'unrelated');
    await navigateWithClock(page, () => page.goForward());
    await results(page, matchingIds, 'needle');
    await home(page, mobile, true);
    await page.locator('button[title="Clear search"]:visible').click();
    await page.clock.runFor(50);
    await page.locator('select[x-model="searchMode"]:visible').selectOption('hybrid');
    await page.clock.runFor(50);
    assert.equal(route(page).path, '#/');
    assert.equal(await page.locator(inputSelector).inputValue(), '');
    await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
  });

  test(`${device}: encoded queries survive reload and empty-result clearing`, async t => {
    const page = await openViewer(t, mobile);
    await page.locator(inputSelector).fill(specialQuery);
    await results(page, ['bv-special'], specialQuery);
    await page.reload();
    await page.locator('[x-show="loading"]').waitFor({ state: 'hidden', timeout: 30000 });
    await searchInput(page, mobile);
    await results(page, ['bv-special'], specialQuery);
    await page.locator(inputSelector).fill('no-such-issue-28');
    await results(page, [], 'no-such-issue-28');
    await listView(page).getByRole('button', { name: 'Clear search', exact: true }).click();
    await results(page, allIds, '');
  });

  test(`${device}: graph Insights show the real blocker and visible what-if counts`, async t => {
    const page = await openViewer(t, mobile, '#/insights');
    await insightsImpact(page, 'bv-open', 2, ['bv-blocked', 'bv-other']);
    const graph = await page.evaluate(() => {
      const viewer = window.beadsViewer;
      const state = viewer.GRAPH_STATE;
      const rank = state.graph.pagerankDefault();
      return {
        actionable: [...viewer.getActionableIssues()].sort(),
        nodes: state.graph.nodeCount(),
        blockerRank: rank[state.nodeMap.get('bv-open')],
        dependentRank: rank[state.nodeMap.get('bv-blocked')],
      };
    });
    assert.deepEqual(graph.actionable, ['bv-description', 'bv-open', 'bv-progress', 'bv-special'],
      'standalone open work is actionable; closed issues and blocked dependents are not');
    assert.equal(graph.nodes, issues.length, 'isolated issues also belong to the graph');
    assert.ok(graph.blockerRank > graph.dependentRank, 'reference-direction PageRank still values the blocker');
  });

  test(`${device}: force graph keeps blocker paths and cascades correct after re-entry`, async t => {
    // Cover both dashboard navigation and a fresh deep link, which initializes
    // the viewer's and force graph's WASM consumers concurrently.
    const page = await openViewer(t, mobile, mobile ? '#/graph' : '#/');
    for (let visit = 0; visit < 2; visit += 1) {
      if (!mobile && visit === 0) {
        await page.getByRole('link', { name: 'Explore dependency graph', exact: true }).click();
      }
      await openForceGraph(page);
      assert.deepEqual(await dependencyPath(page, 'bv-open'), { blockers: 0, dependents: 2 });
      assert.deepEqual(await dependencyPath(page, 'bv-blocked'), { blockers: 1, dependents: 0 });
      const expectedIds = ['bv-blocked', 'bv-other'];
      assert.deepEqual(await forceGraphImpact(page, 'bv-open'), {
        direct: 2, total: 2, ids: expectedIds,
        summary: { direct: 2, total: 2, directIds: expectedIds, ids: expectedIds },
        animated: expectedIds,
      });
      await page.getByText('Closing bv-open would unblock 2 issues directly', { exact: true }).last().waitFor({ state: 'visible' });
      if (mobile) {
        await page.locator('[x-show="view === \'graph\'"]').getByRole('button', { name: 'Back', exact: true }).click();
        await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
      } else {
        await home(page, mobile);
      }
      assert.equal(route(page).path, '#/', 'leaving the graph updates the route before re-entry');
    }
  });
}

test('desktop: graph diamond honors all blockers, resolved statuses and exported vertices', async t => {
  const page = await openViewer(t, false, '#/insights', false, 'graph-fixture/');
  const expectedCascade = ['graph-join', 'graph-left', 'graph-right', 'graph-tail'];
  await insightsImpact(page, 'graph-root', 2, expectedCascade);
  const state = await page.evaluate(() => {
    const viewer = window.beadsViewer;
    return {
      actionable: [...viewer.getActionableIssues()].sort(),
      nodes: [...viewer.GRAPH_STATE.nodeMap.keys()].sort(),
      edges: viewer.GRAPH_STATE.graph.edgeCount(),
      left: viewer.whatIfClose('graph-left'),
      join: viewer.whatIfClose('graph-join'),
    };
  });
  assert.deepEqual(state.actionable, ['graph-external', 'graph-info', 'graph-isolated', 'graph-ready', 'graph-root']);
  assert.deepEqual(state.nodes, graphIssues.map(issue => issue[0]).sort(), 'unknown endpoints do not create phantom work');
  assert.equal(state.edges, 7, 'blocking types participate; related/parent-child and missing endpoints do not');
  assert.equal(state.left.direct_unblocks, 0, 'closing one branch cannot unblock a join with another open prerequisite');
  assert.equal(state.left.transitive_unblocks, 0);
  assert.equal(state.join.direct_unblocks, 1);
  assert.deepEqual(state.join.cascade_issue_ids, ['graph-tail']);

  await page.getByRole('link', { name: 'Issues', exact: true }).click();
  await page.getByRole('button', { name: 'View issue graph-tombstone: Tombstoned prerequisite', exact: true }).click();
  const modal = page.locator('[x-show="selectedIssue"]');
  await modal.waitFor({ state: 'visible' });
  assert.equal(await modal.getByRole('button', { name: 'Simulate Close', exact: true }).isVisible(), false,
    'resolved tombstones do not offer a close simulation');
  await modal.locator('button').first().click();
  await modal.waitFor({ state: 'hidden' });

  await openForceGraph(page);
  assert.deepEqual(await dependencyPath(page, 'graph-root'), { blockers: 0, dependents: 4 });
  assert.deepEqual(await dependencyPath(page, 'graph-join'), { blockers: 3, dependents: 1 });
  assert.deepEqual(await forceGraphImpact(page, 'graph-root'), {
    direct: 2, total: 4, ids: expectedCascade,
    summary: { direct: 2, total: 4, directIds: ['graph-left', 'graph-right'], ids: expectedCascade },
    animated: expectedCascade,
  });
  await page.getByText('Closing graph-root would unblock 2 issues directly, 4 total in cascade', { exact: true }).waitFor({ state: 'visible' });
});

for (const mobile of [false, true]) {
  const device = mobile ? 'mobile' : 'desktop';

  test(`${device}: recorded graph history shows each event without rewriting the current graph`, async t => {
    const page = await openViewer(t, mobile, '#/graph', false, 'graph-fixture/');
    await openForceGraph(page);
    const snapshot = await currentGraphSnapshot(page);
    assert.equal(snapshot.nodes.find(node => node.id === 'graph-closed').status, 'closed');
    const history = page.getByRole('button', { name: 'History', exact: true });
    await history.click();
    assert.equal(await history.getAttribute('aria-pressed'), 'true');
    await page.getByText('Recorded issue events; details and links show the current export.', { exact: true })
      .waitFor({ state: 'visible' });
    const controls = page.locator('#time-travel-controls');
    const slider = controls.getByRole('slider', { name: 'History event', exact: true });
    assert.equal(await slider.getAttribute('min'), '0');
    assert.equal(await slider.getAttribute('max'), String(recordedEvents.length - 1));
    assert.equal(await slider.getAttribute('step'), '1');
    for (let index = 0; index < recordedEvents.length; index += 1) {
      await historyEventVisible(page, recordedEvents[index], index, recordedEvents.length, snapshot);
      if (index + 1 < recordedEvents.length) {
        await controls.getByRole('button', { name: 'Next event', exact: true }).click();
      }
    }
    assert.equal(await controls.getByRole('button', { name: 'Next event', exact: true }).isDisabled(), true);
    await controls.getByRole('button', { name: 'First event', exact: true }).click();
    await historyEventVisible(page, recordedEvents[0], 0, recordedEvents.length, snapshot);
    assert.equal(await controls.getByRole('button', { name: 'Previous event', exact: true }).isDisabled(), true);
    await controls.getByRole('button', { name: 'Last event', exact: true }).click();
    await controls.getByRole('button', { name: 'Previous event', exact: true }).click();
    await historyEventVisible(page, recordedEvents[3], 3, recordedEvents.length, snapshot);
    await controls.getByRole('button', { name: 'Close history', exact: true }).click();
    await controls.waitFor({ state: 'hidden' });
    assert.equal(await history.getAttribute('aria-pressed'), 'false');
    assert.deepEqual(await currentGraphSnapshot(page), snapshot);
    assert.deepEqual(await page.evaluate(async () => (await import('./graph.js')).getTimeTravelState().highlightedIssueIds), []);
  });

  test(`${device}: recorded history playback pauses, restarts and stops on graph exit`, async t => {
    const page = await openViewer(t, mobile, '#/graph', true, 'graph-fixture/');
    await openForceGraph(page);
    await page.getByRole('button', { name: 'History', exact: true }).click();
    await page.clock.pauseAt(new Date('2026-10-07T13:00:00Z'));
    await page.evaluate(() => {
      window.recordedHistorySteps = [];
      document.addEventListener('bv-graph:timeTravelEvent', event => {
        window.recordedHistorySteps.push(event.detail.idx);
      });
    });
    const controls = page.locator('#time-travel-controls');
    await controls.getByRole('button', { name: 'Play history', exact: true }).click();
    await page.clock.runFor(1100);
    assert.deepEqual(await page.evaluate(() => window.recordedHistorySteps), [1]);
    await controls.getByRole('button', { name: 'Pause history', exact: true }).click();
    await page.clock.runFor(2200);
    assert.deepEqual(await page.evaluate(() => window.recordedHistorySteps), [1], 'pause cancels scheduled playback');
    // Restart and pause within one frame, then resume: a canceled RAF must not
    // survive and create a second playback loop when Play is pressed again.
    await controls.getByRole('button', { name: 'Play history', exact: true }).click();
    await controls.getByRole('button', { name: 'Pause history', exact: true }).click();
    await controls.getByRole('button', { name: 'Play history', exact: true }).click();
    await page.clock.runFor(1100);
    assert.deepEqual(await page.evaluate(() => window.recordedHistorySteps), [1, 2]);
    await controls.getByRole('button', { name: 'Close history', exact: true }).click();
    await page.clock.runFor(2200);
    assert.deepEqual(await page.evaluate(() => window.recordedHistorySteps), [1, 2], 'closing history cancels playback');
    await controls.waitFor({ state: 'hidden' });
    await page.getByRole('button', { name: 'History', exact: true }).click();
    assert.equal(await controls.locator('#tt-position').textContent(), `1 / ${recordedEvents.length} events`);
    await controls.getByRole('button', { name: 'Play history', exact: true }).click();
    const beforeExit = await page.evaluate(() => [...window.recordedHistorySteps]);
    if (mobile) {
      await navigateWithClock(page, () => page.locator('[x-show="view === \'graph\'"]')
        .getByRole('button', { name: 'Back', exact: true }).click());
      await page.locator('[x-show="view === \'dashboard\'"]').waitFor({ state: 'visible' });
    } else {
      await home(page, false, true);
    }
    await page.clock.runFor(2200);
    assert.deepEqual(await page.evaluate(() => window.recordedHistorySteps), beforeExit,
      'leaving the graph stops playback and removes controls');
    assert.equal(await page.locator('#time-travel-controls').count(), 0);
    assert.deepEqual(await page.evaluate(async () => {
      const module = await import('./graph.js');
      const state = module.getTimeTravelState();
      return { graph: module.getGraph(), active: state.active, playing: state.playing, events: state.totalEvents };
    }), { graph: null, active: false, playing: false, events: 0 });
  });
}

test('desktop: cleanup cancels a graph initialization across the real WASM promise', async t => {
  const page = await openViewer(t, false);
  const state = await page.evaluate(async () => {
    const module = await import('./graph.js');
    let readyEvents = 0;
    const ready = () => { readyEvents += 1; };
    document.addEventListener('bv-graph:ready', ready);
    // Even cached WASM initialization yields at await. Cleanup in that exact
    // window must invalidate initGraph before it creates a canvas or listeners.
    const pending = module.initGraph('graph-container');
    module.cleanup();
    await pending;
    const observed = {
      graph: Boolean(module.getGraph()), wasm: module.isWasmReady(),
      canvas: document.querySelectorAll('#graph-container canvas').length, readyEvents,
    };
    document.removeEventListener('bv-graph:ready', ready);
    module.cleanup();
    return observed;
  });
  assert.deepEqual(state, { graph: false, wasm: false, canvas: 0, readyEvents: 0 },
    'an obsolete init cannot resurrect graph state after cleanup');
});

test('desktop: history handles single events, wrapped histories, long timelines and absent dates', async t => {
  const single = historyEvent('graph-closed', 'closed', '2026-10-06T09:00:00Z', 'Only dated event');
  let payload = exportedHistory([single]);
  let status = 200;
  const page = await openViewer(t, false, '#/graph', false, 'graph-fixture/', async page => {
    await page.route('**/data/history.json?*', request => request.fulfill({
      status, contentType: 'application/json', body: JSON.stringify(payload),
    }));
  });
  await openForceGraph(page);
  const history = page.getByRole('button', { name: 'History', exact: true });
  await history.click();
  const snapshot = await currentGraphSnapshot(page);
  await historyEventVisible(page, single, 0, 1, snapshot);
  const slider = page.getByRole('slider', { name: 'History event', exact: true });
  assert.equal(await slider.getAttribute('max'), '0');
  assert.equal(await slider.inputValue(), '0', 'one event never produces NaN or a fractional index');
  for (const name of ['First event', 'Previous event', 'Next event', 'Last event', 'Play history']) {
    assert.equal(await page.getByRole('button', { name, exact: true }).isDisabled(), true, `${name} has no further event`);
  }

  const events = Array.from({ length: 103 }, (_, index) => historyEvent(
    index % 2 ? 'graph-root' : 'graph-closed', index === 0 ? 'reopened' : 'updated',
    new Date(Date.UTC(2026, 9, 6, 10, 0, index)).toISOString(), `Recorded event ${index}`, 'shared-commit',
  ));
  await home(page, false);
  payload = { histories: structuredClone(exportedHistory(events)) };
  delete payload.histories['graph-root'].events[0].bead_id;
  delete payload.histories['graph-closed'].bead_id;
  delete payload.histories['graph-closed'].events[0].bead_id;
  await openForceGraph(page);
  await history.click();
  const longSnapshot = await currentGraphSnapshot(page);
  assert.equal(await slider.getAttribute('max'), '102');
  assert.equal(await slider.getAttribute('step'), '1');
  await historyEventVisible(page, events[0], 0, events.length, longSnapshot);
  await selectHistoryEvent(page, 1);
  await historyEventVisible(page, events[1], 1, events.length, longSnapshot);
  await selectHistoryEvent(page, 101);
  await historyEventVisible(page, events[101], 101, events.length, longSnapshot);
  await slider.press('ArrowRight');
  await historyEventVisible(page, events[102], 102, events.length, longSnapshot);
  await slider.press('ArrowLeft');
  await historyEventVisible(page, events[101], 101, events.length, longSnapshot);
  assert.equal(longSnapshot.nodes.find(node => node.id === 'graph-closed').status, 'closed',
    'a recorded reopening does not invent a different current status');

  // The graph module's explicit commit-timeline input remains usable too.
  // Distinct changes in one commit are separate recorded issue events.
  const commitEvents = [
    historyEvent('graph-root', 'modified', '2026-10-06T12:00:00Z', 'Shared commit', 'known-sha'),
    historyEvent('graph-left', 'claimed', '2026-10-06T12:00:00Z', 'Shared commit', 'known-sha'),
    historyEvent('graph-closed', 'reopened', '2026-10-06T12:00:00Z', 'Shared commit', 'known-sha'),
  ];
  await home(page, false);
  payload = { commits: [{
    sha: 'known-sha', date: '2026-10-06T12:00:00Z', message: 'Shared commit',
    beads_modified: ['graph-root'], beads_claimed: ['graph-left'], beads_reopened: ['graph-closed'],
  }] };
  await openForceGraph(page);
  await history.click();
  for (let index = 0; index < commitEvents.length; index += 1) {
    await selectHistoryEvent(page, index);
    await historyEventVisible(page, commitEvents[index], index, commitEvents.length, longSnapshot);
  }

  for (const missing of [false, true]) {
    await home(page, false);
    status = missing ? 404 : 200;
    payload = exportedHistory([
      historyEvent('graph-root', 'created', '', 'Undated'),
      historyEvent('graph-left', 'updated', 'not-a-date', 'Invalid'),
      historyEvent('unknown-issue', 'created', '2026-10-06T09:00:00Z', 'Absent from export'),
    ]);
    await openForceGraph(page);
    await page.waitForFunction(() => !Alpine.$data(document.querySelector('[x-data="beadsApp()"]')).graphHistoryLoading);
    await history.waitFor({ state: 'visible' });
    assert.equal(await history.isDisabled(), true, missing ? '404 history is unavailable' : 'undated events do not invent a timeline');
    assert.equal(await history.getAttribute('aria-pressed'), 'false');
    assert.equal(await page.locator('#time-travel-controls').count(), 0);
    assert.equal(await page.evaluate(async () => (await import('./graph.js')).getTimeTravelState().totalEvents), 0,
      'previous history cannot leak into an export with no dated events');
    assert.deepEqual(await currentGraphSnapshot(page), snapshot);
  }
});

test('desktop: obsolete history response cannot overwrite a graph reopened before it arrives', async t => {
  let releaseObsolete;
  const obsoleteGate = new Promise(resolve => { releaseObsolete = resolve; });
  let observedRequest;
  const firstRequested = new Promise(resolve => { observedRequest = resolve; });
  let obsoleteFinished;
  const firstFinished = new Promise(resolve => { obsoleteFinished = resolve; });
  const newerEvents = [
    historyEvent('graph-right', 'created', '2026-10-06T11:00:00Z', 'New graph visit'),
    historyEvent('graph-tail', 'updated', '2026-10-06T11:01:00Z', 'Newer tail event'),
  ];
  let requests = 0;
  const page = await openViewer(t, false, '#/graph', false, 'graph-fixture/', async page => {
    await page.route('**/data/history.json?*', async request => {
      requests += 1;
      if (requests === 1) {
        observedRequest();
        await obsoleteGate;
        await request.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(recordedHistory) });
        const response = await request.request().response();
        if (response) await response.finished();
        obsoleteFinished();
      } else {
        await request.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(exportedHistory(newerEvents)) });
      }
    });
  });
  t.after(() => releaseObsolete());
  await firstRequested;
  await page.locator('#graph-container canvas').first().waitFor({ state: 'visible' });
  await home(page, false);
  await openForceGraph(page);
  const history = page.getByRole('button', { name: 'History', exact: true });
  await history.click();
  const snapshot = await currentGraphSnapshot(page);
  await historyEventVisible(page, newerEvents[0], 0, newerEvents.length, snapshot);
  assert.equal(requests, 2, 're-entry starts its own history request immediately');
  releaseObsolete();
  await firstFinished;
  // Let native fetch/JSON continuations and the ensuing paint complete; no
  // arbitrary sleep or mocked application state decides this response race.
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await historyEventVisible(page, newerEvents[0], 0, newerEvents.length, snapshot);
  assert.equal(await page.locator('#time-travel-controls').count(), 1);
  assert.equal(route(page).path, '#/graph');
});
