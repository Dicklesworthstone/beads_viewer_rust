// Run with: node --test scripts/test_viewer_search.cjs
// Requires Playwright and its Chromium browser; no package manifest changes.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const { createServer } = require('node:http');
const { readFileSync } = require('node:fs');
const { resolve, join } = require('node:path');
const { spawnSync } = require('node:child_process');

const root = resolve(__dirname, '..');
// Use the exporter's real schema and bundled sql.js, deliberately without FTS.
const fixture = spawnSync('python3', ['-c', `
import sqlite3, re, sys
source = open(sys.argv[1]).read()
schema = re.search(r'CREATE TABLE IF NOT EXISTS issues \\(.*?CREATE INDEX IF NOT EXISTS idx_mv_score.*?;', source, re.S).group()
db = sqlite3.connect(':memory:')
db.executescript(schema)
for table in ('issues', 'issue_overview_mv'):
    for id, title, status in [('bd-1', 'Needle repair', 'open'), ('bd-2', 'Needle archive', 'closed'), ('bd-3', 'Other task', 'open')]:
        db.execute(f'INSERT INTO {table} (id,title,status,priority,issue_type,created_at,updated_at) VALUES (?,?,?,2,"task","2026-10-06","2026-10-06")', (id,title,status))
sys.stdout.buffer.write(db.serialize())
`, join(root, 'src/export_sqlite.rs')]);
assert.equal(fixture.status, 0, fixture.stderr.toString());

for (const mobile of [false, true]) {
  test(`dashboard search and route state (${mobile ? 'mobile' : 'desktop'})`, async () => {
    const server = createServer((req, res) => {
      res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
      res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
      const path = new URL(req.url, 'http://localhost').pathname;
      if (path === '/beads.sqlite3') return res.end(fixture.stdout);
      try {
        const file = join(root, 'viewer_assets', path === '/' ? 'index.html' : path);
        const types = { html: 'text/html', js: 'text/javascript', css: 'text/css', wasm: 'application/wasm' };
        res.setHeader('Content-Type', types[file.split('.').pop()] || 'application/octet-stream');
        res.end(readFileSync(file));
      } catch { res.writeHead(404).end(); }
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    let browser;
    try {
      browser = await chromium.launch({
        headless: true, executablePath: process.env.CHROMIUM_PATH,
        args: process.env.CHROMIUM_PATH ? ['--no-zygote', '--use-gl=angle', '--use-angle=swiftshader'] : [],
      });
      const page = await browser.newPage({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 } });
      if (process.env.VIEWER_TEST_DEBUG) {
        page.on('console', msg => console.log(msg.text()));
        page.on('pageerror', error => console.error(error.message));
      }
      const url = `http://127.0.0.1:${server.address().port}/`;
      const state = fn => page.evaluate(fn);
      const wait = fn => page.waitForFunction(fn);
      await page.goto(url, { waitUntil: 'domcontentloaded' });
      await wait(() => !Alpine.$data(document.querySelector('[x-data]')).loading);
      if (mobile) await page.locator('button[\\@click="mobileSearchOpen = !mobileSearchOpen"]').click();
      const input = page.locator('input[placeholder="Search issues..."]:visible');
      await input.fill('Needle');
      await wait(() => location.hash.includes('q=Needle') && Alpine.$data(document.querySelector('[x-data]')).view === 'issues');
      await page.locator('[x-text="issue.title"]:visible').filter({ hasText: 'Needle repair' }).first().waitFor();
      assert.equal(await state(() => Alpine.$data(document.querySelector('[x-data]')).totalIssues), 2);

      // Navigation controls must carry search, including a click during debounce.
      await page.locator('a[href="#/"]:visible').first().click();
      await input.fill('repair');
      await page.locator('a[href="#/issues"]:visible').first().click();
      await wait(() => location.hash.includes('q=repair'));
      await wait(() => Alpine.$data(document.querySelector('[x-data]')).totalIssues === 1);
      await input.fill('Needle');
      await wait(() => location.hash.includes('q=Needle'));
      await page.locator('a[href="#/insights"]:visible').first().click();
      await page.locator('a[href="#/issues"]:visible').first().click();
      assert.equal(await input.inputValue(), 'Needle');

      // History must restore complete snapshots, clearing absent filters.
      await page.evaluate(() => { location.hash = '/issues?status=open&q=Needle'; });
      await wait(() => Alpine.$data(document.querySelector('[x-data]')).totalIssues === 1);
      await page.evaluate(() => { location.hash = '/issues?q=Needle'; });
      await wait(() => Alpine.$data(document.querySelector('[x-data]')).totalIssues === 2);
      await page.goBack();
      await wait(() => Alpine.$data(document.querySelector('[x-data]')).filters.status[0] === 'open');
      await page.goForward();
      await wait(() => Alpine.$data(document.querySelector('[x-data]')).filters.status.length === 0);
      assert.equal(await input.inputValue(), 'Needle');

      // Dashboard filter shortcuts and issue modal round-trips retain the query.
      await page.locator('a[href="#/"]:visible').first().click();
      await page.locator('[\\@click="filters.status = [\'open\']; view = \'issues\'; applyFilters()"]:visible').first().click();
      await wait(() => location.hash.includes('status=open') && location.hash.includes('q=Needle'));
      await page.locator('[x-text="issue.title"]:visible').filter({ hasText: 'Needle repair' }).first().click();
      await wait(() => !!Alpine.$data(document.querySelector('[x-data]')).selectedIssue);
      await state(() => Alpine.$data(document.querySelector('[x-data]')).closeIssue());
      await wait(() => !Alpine.$data(document.querySelector('[x-data]')).selectedIssue);
      assert.equal(await input.inputValue(), 'Needle');
      await page.reload();
      await wait(() => !Alpine.$data(document.querySelector('[x-data]')).loading);
      if (mobile) await page.locator('button[\\@click="mobileSearchOpen = !mobileSearchOpen"]').click();
      assert.equal(await input.inputValue(), 'Needle');
      await page.locator('button[title="Clear search"]:visible').click();
      await wait(() => !location.hash.includes('q=') && Alpine.$data(document.querySelector('[x-data]')).totalIssues === 2);
      await input.fill('no-such-needle');
      await page.getByText('No results for', { exact: false }).waitFor({ state: 'visible' });
    } catch (error) {
      console.error(error);
      throw error;
    } finally {
      if (browser) await browser.close();
      await new Promise(resolve => server.close(resolve));
    }
  });
}
