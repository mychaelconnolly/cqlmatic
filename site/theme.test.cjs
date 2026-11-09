const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { runInNewContext } = require('node:vm');

const source = readFileSync(join(__dirname, 'public/assets/theme.js'), 'utf8');

function load({ saved, dark = false, denyRead = false, denyWrite = false } = {}) {
  const events = {};
  const attrs = {};
  const icons = ['dark', 'light', 'auto'].map(themeIcon => {
    const attributes = new Set(themeIcon === 'dark' ? [] : ['hidden']);
    return {
      dataset: { themeIcon },
      hasAttribute: name => attributes.has(name),
      toggleAttribute: (name, present) => { if (present) attributes.add(name); else attributes.delete(name); },
    };
  });
  const button = {
    hidden: true,
    setAttribute: (name, value) => { attrs[name] = value; },
    querySelectorAll: () => icons,
    addEventListener: (name, callback) => { events[name] = callback; },
  };
  const meta = { setAttribute: (name, value) => { meta[name] = value; } };
  const root = { dataset: {} };
  const storage = { value: saved };
  const system = { matches: dark, addEventListener: (name, callback) => { events[name] = callback; } };
  runInNewContext(source, {
    window: { matchMedia: () => system },
    localStorage: {
      getItem: () => { if (denyRead) throw new Error('denied'); return storage.value; },
      setItem: (key, value) => { if (denyWrite) throw new Error('denied'); storage.value = value; },
    },
    document: {
      documentElement: root,
      querySelector: selector => selector === '.theme-toggle' ? button : meta,
      addEventListener: (name, callback) => { events[name] = callback; },
    },
  });
  return {
    root, button, attrs, icons, meta, storage,
    attach: () => events.DOMContentLoaded(),
    click: () => events.click(),
    os: value => { system.matches = value; events.change(); },
  };
}

function state(page, mode, theme, next) {
  assert.equal(page.root.dataset.themePreference, mode);
  assert.equal(page.root.dataset.theme, theme);
  assert.equal(page.meta.content, theme === 'dark' ? '#151517' : '#ffffff');
  assert.equal(page.attrs['aria-label'], `Theme: ${mode}. Switch to ${next}.`);
  assert.equal(page.attrs.title, page.attrs['aria-label']);
  assert.deepEqual(page.icons.filter(icon => !icon.hasAttribute('hidden')).map(icon => icon.dataset.themeIcon), [mode]);
}

test('default is dark before controls attach, independent of OS', () => {
  const page = load();
  assert.equal(page.root.dataset.theme, 'dark');
  assert.equal(page.meta.content, '#151517');
  assert.equal(page.button.hidden, true);
  page.attach();
  assert.equal(page.button.hidden, false);
  state(page, 'dark', 'dark', 'light');
});

test('one button cycles dark to light to auto to dark and persists choices', () => {
  const page = load(); page.attach();
  for (const [mode, theme, next] of [['light', 'light', 'auto'], ['auto', 'light', 'dark'], ['dark', 'dark', 'light']]) {
    page.click();
    state(page, mode, theme, next);
    assert.equal(page.storage.value, mode);
    const reloaded = load({ saved: page.storage.value }); reloaded.attach();
    state(reloaded, mode, theme, next);
  }
});

test('stored choices apply before stylesheet loading and survive reload', () => {
  for (const saved of ['dark', 'light', 'auto']) {
    const page = load({ saved, dark: true });
    assert.equal(page.root.dataset.themePreference, saved);
    assert.equal(page.root.dataset.theme, saved === 'light' ? 'light' : 'dark');
    page.attach();
    state(page, saved, saved === 'light' ? 'light' : 'dark', saved === 'dark' ? 'light' : saved === 'light' ? 'auto' : 'dark');
  }
});

test('auto follows OS changes and leaving auto stops OS overrides', () => {
  const page = load({ saved: 'auto' }); page.attach();
  state(page, 'auto', 'light', 'dark');
  page.os(true); state(page, 'auto', 'dark', 'dark');
  page.os(false); state(page, 'auto', 'light', 'dark');
  page.click(); state(page, 'dark', 'dark', 'light');
  page.os(true); page.os(false); state(page, 'dark', 'dark', 'light');
  page.click(); page.os(true); state(page, 'light', 'light', 'auto');
});

test('explicit stored modes ignore OS changes', () => {
  for (const saved of ['dark', 'light']) {
    const page = load({ saved }); page.attach();
    page.os(true); page.os(false);
    state(page, saved, saved, saved === 'dark' ? 'light' : 'auto');
  }
});

test('invalid preferences fall back to dark', () => {
  for (const saved of ['system', '', 'LIGHT', null, '{}']) {
    const page = load({ saved }); page.attach();
    state(page, 'dark', 'dark', 'light');
  }
});

test('denied storage keeps a working dark default and in-session controls', () => {
  const page = load({ saved: 'light', denyRead: true, denyWrite: true }); page.attach();
  state(page, 'dark', 'dark', 'light');
  page.click(); state(page, 'light', 'light', 'auto');
  page.click(); page.os(true); state(page, 'auto', 'dark', 'dark');
});

test('both documents load theme before CSS and hide the control without JS', () => {
  for (const name of ['index.html', '404.html']) {
    const html = readFileSync(join(__dirname, 'public', name), 'utf8');
    assert.ok(html.indexOf('<script src="/assets/theme.js"></script>') < html.indexOf('<link rel="stylesheet"'));
    assert.match(html, /<html[^>]+data-theme="dark"/);
    assert.equal((html.match(/<button\b/g) || []).length, name === 'index.html' ? 2 : 1);
    assert.match(html, /<button[^>]+class="theme-toggle"[^>]+\bhidden\b/);
    assert.doesNotMatch(html, /coming soon|pending publication/i);
  }
  const css = readFileSync(join(__dirname, 'public/assets/site.css'), 'utf8');
  assert.match(css, /\[hidden\]\s*\{\s*display:\s*none\s*!important;/);
  assert.match(css, /\.theme-toggle\s*\{[^}]*width:\s*44px;[^}]*height:\s*44px;/);
});

test('opening award credit identifies the agent and keeps documentation discoverable', () => {
  const html = readFileSync(join(__dirname, 'public/index.html'), 'utf8');
  const readme = readFileSync(join(__dirname, '../README.md'), 'utf8');
  for (const document of [html, readme]) {
    assert.equal((document.match(/src="[^"\n]*agentworks-expert-builder-2026\.png"/g) || []).length, 1);
    assert.match(document, /alt="[^"]*awarded to the &quot;THIS IS CQL&quot; agent"/);
    assert.match(document, /November 2, 2026 hackathon winners announcement/);
    assert.doesNotMatch(document, /## Project background|cqlmatic-windows-x86_64\.exe/);
  }
  assert.ok(html.indexOf('class="recognition"') < html.indexOf('class="code-example"'));
  assert.match(html, /href="https:\/\/docs\.cqlmatic\.dev\/"/);
  assert.match(readme, /^# CQLmatic\n/);
  const backgroundPosition = readme.indexOf("CQLmatic's query mechanics");
  const badgePosition = readme.indexOf('src="site/public/assets/agentworks-expert-builder-2026.png"');
  const enginePosition = readme.indexOf('The same engine powers');
  assert.ok(backgroundPosition >= 0 && backgroundPosition < badgePosition && badgePosition < enginePosition);
  assert.match(readme, /agentworks-expert-builder-2026\.png" width="250"/);
  assert.match(readme, /<a id="project-background"><\/a>/);
  assert.match(readme, /\[Read the setup guide\]\(https:\/\/docs\.cqlmatic\.dev\/setup\/\)/);
  for (const name of ['index.html', '404.html']) {
    const document = readFileSync(join(__dirname, 'public', name), 'utf8');
    const wordmark = document.match(/<a class="wordmark"[^>]*>(.*?)<\/a>/s);
    assert.ok(wordmark);
    assert.doesNotMatch(wordmark[1], /<img\b/);
    assert.match(document, /href="\/assets\/favicon\.svg"/);
  }
});
