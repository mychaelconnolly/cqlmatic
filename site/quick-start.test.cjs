const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { runInNewContext } = require('node:vm');

const source = readFileSync(join(__dirname, 'public/assets/quick-start.js'), 'utf8');

function load(writeText) {
  let click;
  const button = { hidden: true, addEventListener: (name, callback) => { if (name === 'click') click = callback; } };
  const command = { textContent: 'npx cqlmatic@latest setup' };
  const status = { textContent: '' };
  const elements = { '.copy-command': button, '#quick-start-command': command, '.copy-status': status };
  runInNewContext(source, {
    document: { querySelector: selector => elements[selector] },
    navigator: { clipboard: writeText ? { writeText } : undefined },
  });
  return { button, status, click };
}

test('copies the visible command and reports clipboard outcomes', async () => {
  let copied;
  const ready = load(text => { copied = text; });
  assert.equal(ready.button.hidden, false);
  await ready.click();
  assert.equal(copied, 'npx cqlmatic@latest setup');
  assert.equal(ready.status.textContent, 'Copied to clipboard.');

  const denied = load(() => { throw new Error('denied'); });
  await denied.click();
  assert.match(denied.status.textContent, /Select the command and copy it/);

  const unavailable = load();
  assert.equal(unavailable.button.hidden, true);
  assert.equal(unavailable.click, undefined);
});
