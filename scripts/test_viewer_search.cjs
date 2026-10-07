#!/usr/bin/env node
'use strict';

// Run with an existing Playwright installation (no npm project is needed):
// NODE_PATH=/path/to/node_modules node --test scripts/test_viewer_search.cjs
// BVR_CHROMIUM_EXECUTABLE (or CHROMIUM_PATH) selects an installed Chromium.
// BVR_VIEWER_REVISION optionally serves index.html/viewer.js from a git revision
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
const allIds = issues.map(issue => issue[0]);
const matchingIds = ['bv-open', 'bv-blocked', 'bv-progress', 'bv-closed', 'bv-description'];
const openIds = ['bv-open', 'bv-blocked', 'bv-description'];
const rowSelector = '[role="button"][aria-label^="View issue "]';
const inputSelector = 'input[placeholder="Search issues..."]:visible';
const executablePath = process.env.BVR_CHROMIUM_EXECUTABLE || process.env.CHROMIUM_PATH;
let server;
let origin;

async function fixtureDatabase() {
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
  db.run("INSERT INTO export_meta VALUES ('issue_count', ?)", [String(issues.length)]);
  for (const issue of issues) {
    db.run(`INSERT INTO issue_overview_mv
      (id, title, description, status, priority, issue_type, assignee, labels,
       blocked_by_ids, blocks_count, triage_score, created_at, updated_at)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0.5, '2026-10-06T12:00:00Z', '2026-10-06T12:00:00Z')`, issue);
  }
  db.run("INSERT INTO dependencies (issue_id, depends_on_id) VALUES ('bv-blocked', 'bv-open'), ('bv-other', 'bv-open')");
  // Deliberately omit issues_fts: the shipped LIKE fallback must work without FTS5,
  // as it does in the original report. No application/search/router is mocked.
  const bytes = Buffer.from(db.export());
  db.close();
  return bytes;
}

before(async () => {
  const database = await fixtureDatabase();
  const overrides = new Map();
  if (process.env.BVR_VIEWER_REVISION) {
    for (const file of ['index.html', 'viewer.js']) {
      overrides.set(file, execFileSync('git', ['show', `${process.env.BVR_VIEWER_REVISION}:viewer_assets/${file}`], { cwd: root }));
    }
  }
  const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm' };
  server = http.createServer(async (request, response) => {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    response.setHeader('Cache-Control', 'no-store');
    // Supply the isolation headers directly, as a configured preview server can,
    // so COI service-worker installation does not reload a test mid-interaction.
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'credentialless');
    if (pathname === '/beads.sqlite3') {
      response.writeHead(200, { 'Content-Type': 'application/octet-stream' });
      response.end(database);
      return;
    }
    if (pathname === '/beads.sqlite3.config.json') {
      response.writeHead(200, { 'Content-Type': 'application/json' });
      response.end('{}');
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

async function openViewer(t, mobile, hash = '#/', useClock = false) {
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
  await page.goto(origin + '/' + hash);
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
}
