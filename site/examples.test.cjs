const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { runInNewContext } = require('node:vm');

const source = readFileSync(join(__dirname, 'public/assets/examples.js'), 'utf8');

function makeClassList(initial = []) {
  const set = new Set(initial);
  return {
    add: (...names) => names.forEach(n => set.add(n)),
    remove: (...names) => names.forEach(n => set.delete(n)),
    toggle: (name, force) => {
      const present = force === undefined ? !set.has(name) : Boolean(force);
      if (present) set.add(name);
      else set.delete(name);
      return present;
    },
    contains: name => set.has(name),
  };
}

function makeElement(dataset = {}) {
  const attrs = {};
  const listeners = {};
  const children = [];
  return {
    dataset,
    attrs,
    children,
    hidden: true,
    inert: false,
    textContent: '',
    offsetWidth: 100,
    classList: makeClassList(),
    setAttribute: (k, v) => { attrs[k] = String(v); },
    getAttribute: k => attrs[k],
    appendChild: child => { children.push(child); return child; },
    addEventListener: (name, cb) => { listeners[name] = cb; },
    trigger: (name, event = {}) => listeners[name]?.(event),
    contains: target => children.includes(target),
  };
}

function load({ reducedMotion = false } = {}) {
  const slides = ['PHP2Shell', 'Node2Shell', 'Post-exploit', 'Outbound'].map((label, i) => {
    const el = makeElement({ exampleLabel: label });
    el.offsetHeight = 600 + i * 20;
    return el;
  });
  const controls = makeElement();
  const progress = makeElement();
  const body = makeElement();
  body.style = {};
  body.offsetHeight = 648;
  const container = makeElement();
  container.querySelectorAll = sel => sel === '.example-slide' ? slides : [];
  container.querySelector = sel => sel === '.example-controls' ? controls : sel === '.example-progress' ? progress : sel === '.code-body' ? body : null;

  let nextTimerId = 1;
  const timers = new Map();
  const docListeners = {};
  const doc = {
    hidden: false,
    querySelector: sel => sel === '.code-example' ? container : null,
    createElement: () => makeElement(),
    addEventListener: (name, cb) => { docListeners[name] = cb; },
  };
  const motion = { matches: reducedMotion, addEventListener: () => {} };

  runInNewContext(source, {
    document: doc,
    window: { matchMedia: () => motion },
    setTimeout: (fn, ms) => {
      const id = nextTimerId++;
      timers.set(id, { fn, ms });
      return id;
    },
    clearTimeout: id => { timers.delete(id); },
  });

  function flush(minMs = 0) {
    const due = [...timers.entries()].filter(([, t]) => t.ms >= minMs).sort((a, b) => a[1].ms - b[1].ms);
    if (!due.length) return false;
    const [id, t] = due[0];
    timers.delete(id);
    t.fn();
    return true;
  }

  return { container, slides, controls, progress, doc, docListeners, timers, flush };
}

test('initializes rotating tabs, exposes first slide, and cross-dissolves on advance', () => {
  const page = load();
  assert.equal(page.container.classList.contains('is-ready'), true);
  assert.equal(page.controls.hidden, false);
  assert.equal(page.controls.children.length, 4);
  assert.equal(page.slides[0].classList.contains('is-active'), true);
  assert.equal(page.slides[0].inert, false);
  assert.equal(page.slides[1].inert, true);
  assert.equal(page.progress.classList.contains('is-ticking'), true);

  // Fire auto-rotate timer (9500ms) -> outgoing dissolves, then incoming enters, then settles.
  assert.equal(page.flush(9000), true);
  assert.equal(page.slides[0].classList.contains('is-leaving'), true);
  assert.equal(page.slides[0].inert, true);
  assert.equal(page.controls.children[1].getAttribute('aria-selected'), 'true');

  assert.equal(page.flush(0), true); // enter timer (110ms)
  assert.equal(page.slides[1].classList.contains('is-active'), true);
  assert.equal(page.slides[1].classList.contains('is-entering'), true);
  assert.equal(page.slides[1].inert, false);

  assert.equal(page.flush(0), true); // settle timer (640ms)
  assert.equal(page.slides[0].classList.contains('is-leaving'), false);
  assert.equal(page.slides[1].classList.contains('is-entering'), false);
});

test('pauses rotation on hover and honors reduced motion without auto-rotating', () => {
  const page = load();
  page.container.trigger('mouseenter');
  assert.equal(page.container.classList.contains('is-paused'), true);
  assert.equal(page.timers.size, 0);
  page.container.trigger('mouseleave');
  assert.equal(page.container.classList.contains('is-paused'), false);
  assert.equal(page.timers.size, 1);

  const reduced = load({ reducedMotion: true });
  assert.equal(reduced.container.classList.contains('is-paused'), true);
  assert.equal(reduced.timers.size, 0);
  reduced.controls.children[2].trigger('click');
  assert.equal(reduced.slides[2].classList.contains('is-active'), true);
  assert.equal(reduced.slides[2].classList.contains('is-entering'), false);
  assert.equal(reduced.slides[0].classList.contains('is-leaving'), false);
});
