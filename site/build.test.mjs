import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { buildSite, DOCUMENTS, documentRegistry, HOSTS, renderMarkdown, requireCommit } from './build.mjs';

const COMMIT = '1234567890abcdef1234567890abcdef12345678';
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const write = (root, path, value) => { mkdirSync(dirname(join(root, path)), { recursive: true }); writeFileSync(join(root, path), value); };
const files = root => readdirSync(root, { recursive: true, withFileTypes: true }).filter(entry => entry.isFile()).map(entry => join(entry.parentPath, entry.name).slice(root.length + 1).replaceAll('\\', '/')).sort();

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'cqlmatic-site-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const document of DOCUMENTS) write(root, document.source, `# ${document.source === 'docs/SETUP.md' ? 'Setup' : document.label}\n\nSynthetic documentation.\n`);
  for (const asset of ['favicon.svg', 'theme.js', 'site.css', 'docs.css', 'github-mark.svg', 'github-mark.LICENSE.txt', 'agentworks-expert-builder-2026.png', 'setup-discovery-query.png']) write(root, `site/public/assets/${asset}`, `synthetic ${asset}\n`);
  write(root, 'site/public/index.html', '<!doctype html><html lang="en"><head><meta name="description" content="Synthetic"><link rel="canonical" href="https://cqlmatic.dev/"><script src="/assets/theme.js"></script><link rel="stylesheet" href="/assets/site.css"><link rel="icon" href="/assets/favicon.svg"></head><body><h1 id="home">CQLmatic</h1><a href="https://docs.cqlmatic.dev/setup/#setup">Docs</a></body></html>');
  write(root, 'site/public/404.html', '<!doctype html><html lang="en"><head><meta name="robots" content="noindex"><link rel="stylesheet" href="/assets/site.css"></head><body><h1>Page not found</h1><a href="/">Home</a></body></html>');
  write(root, 'site/public/_headers', '/*\n  Content-Security-Policy: default-src \'none\'; img-src \'self\'\n');
  write(root, 'site/public/_redirects', '/docs https://docs.cqlmatic.dev/ 301\n/docs/ https://docs.cqlmatic.dev/ 301\n/docs/* https://docs.cqlmatic.dev/:splat 301\n');
  write(root, 'examples/sample.json', '{}\n');
  write(root, 'schemas/sample.json', '{}\n');
  write(root, 'src/lib.rs', 'pub fn sample() {}\n');
  write(root, 'data/sample.json', '{}\n');
  write(root, 'LICENSE', 'Synthetic license\n');
  write(root, 'site/UNLISTED.md', '# Not an input\n');
  return root;
}

test('only exact source commits and approved, unique documentation routes are accepted', () => {
  for (const invalid of [undefined, '', 'main', 'v0.1.3', 'a'.repeat(39), 'g'.repeat(40)]) assert.throws(() => requireCommit(invalid), /40-character/);
  assert.equal(requireCommit(COMMIT.toUpperCase()), COMMIT);
  assert.equal(requireCommit('0'.repeat(40)), '0'.repeat(40));
  assert.equal(DOCUMENTS.length, 29);
  assert.equal(documentRegistry().sources.size, 29);
  assert.throws(() => documentRegistry([DOCUMENTS[0], DOCUMENTS[0]]), /Duplicate/);
  assert.throws(() => documentRegistry([{ source: 'site/UNLISTED.md', route: '/unlisted/' }]), /Unapproved/);
  assert.throws(() => documentRegistry([{ ...DOCUMENTS[0], route: '/changed/' }]), /Unapproved/);
});

test('headings use fresh GitHub slugs from text, with explicit HTML anchors preserved', () => {
  const markdown = '# **Hello** `code` <em>text</em> &amp; [links](#x)\n\n## Duplicate\n\n## Duplicate\n\n<a id="project-background"></a>\n\n<a name="old-anchor"></a>\n';
  const first = renderMarkdown('README.md', markdown);
  assert.deepEqual(first.headings.map(heading => heading.id), ['hello-code-text--links', 'duplicate', 'duplicate-1']);
  assert.equal(first.headings[0].text, 'Hello code text & links');
  assert.ok(first.anchors.has('project-background'));
  assert.ok(first.anchors.has('old-anchor'));
  assert.match(first.html, /<a id="project-background"><\/a>/);
  assert.equal(renderMarkdown('README.md', '## Duplicate\n').headings[0].id, 'duplicate');
  assert.throws(() => renderMarkdown('README.md', '# Duplicate\n\n<a id="duplicate"></a>'), /Duplicate anchor/);
  assert.throws(() => renderMarkdown('site/UNLISTED.md', '# Not allowed'), /Unapproved/);
});

test('only agent-skill frontmatter is omitted, while source bytes are untouched', () => {
  const markdown = '---\nname: synthetic-skill\ndescription: not rendered\n---\n\n# Agent workflow\n';
  assert.doesNotMatch(renderMarkdown('integrations/agent/SKILL.md', markdown).html, /synthetic-skill|not rendered/);
  assert.match(renderMarkdown('README.md', markdown).html, /synthetic-skill/);
  assert.throws(() => renderMarkdown('integrations/agent/SKILL.md', '---\nname: missing-end'), /Unclosed frontmatter/);
});

test('build rebases Markdown and raw HTML, binds GitHub links, and creates deterministic complete checksums', t => {
  const root = fixture(t);
  write(root, 'README.md', `# Project\n\n<a id="project-background"></a>\n\n[Setup](docs/SETUP.md#setup) · [Explicit](#project-background) · [Example](examples/sample.json) · [Schemas](schemas/) · [License](LICENSE) · [Source](src/lib.rs#L1)\n\n<a href="docs/SETUP.md#setup">Raw guide</a>\n<p align="center"><picture><source srcset="site/public/assets/setup-discovery-query.png 1x, site/public/assets/setup-discovery-query.png 2x"><img src="site/public/assets/agentworks-expert-builder-2026.png" width="300" height="300" alt="Synthetic &quot;badge&quot;"></picture><br>Recognition for <strong>the agent</strong>.</p>\n\n[Tagged example](https://github.com/mychaelconnolly/cqlmatic/blob/v0.1.3/examples/sample.json)\n\n| Column | Value |\n| --- | --- |\n| code | value |\n\n\`\`\`sh\necho synthetic\n\`\`\`\n`);
  const protectedPaths = ['README.md', 'docs/ANALYST-WORKFLOW.md', 'src/lib.rs', 'schemas/sample.json', 'data/sample.json'];
  const before = protectedPaths.map(path => digest(readFileSync(join(root, path))));
  const first = buildSite({ root, commit: COMMIT });
  const dist = join(root, 'site/dist');
  const project = readFileSync(join(dist, 'docs/project/index.html'), 'utf8');
  assert.equal(first.documentationPages, 29);
  assert.match(project, /href="\/setup\/#setup"/);
  assert.match(project, /href="#project-background"/);
  assert.match(project, /src="\/assets\/agentworks-expert-builder-2026.png" width="300" height="300"/);
  assert.match(project, /srcset="\/assets\/setup-discovery-query.png 1x, \/assets\/setup-discovery-query.png 2x"/);
  assert.match(project, /Recognition for <strong>the agent<\/strong>/);
  assert.match(project, new RegExp(`href="https://github.com/mychaelconnolly/cqlmatic/blob/${COMMIT}/examples/sample.json"`));
  assert.match(project, new RegExp(`href="https://github.com/mychaelconnolly/cqlmatic/tree/${COMMIT}/schemas"`));
  assert.match(project, new RegExp(`href="https://github.com/mychaelconnolly/cqlmatic/blob/${COMMIT}/src/lib.rs#L1"`));
  assert.match(project, /<pre tabindex="0">/);
  assert.match(project, /class="docs-table" role="region" aria-label="Reference table" tabindex="0"/);
  assert.match(project, /href="\/project\/" aria-current="page"/);
  assert.match(project, /<link rel="canonical" href="https:\/\/docs.cqlmatic.dev\/project\/">/);
  assert.match(project, /<meta property="og:url" content="https:\/\/docs.cqlmatic.dev\/project\/">/);
  assert.match(project, /<a class="skip-link" href="#main">/);
  assert.match(project, /<main id="main"[^>]*tabindex="-1"/);
  assert.match(project, /meta name="theme-color"/);
  assert.equal((project.match(/<button\b/g) || []).length, 1);
  assert.equal((project.match(/data-theme-icon=/g) || []).length, 3);
  assert.ok(project.indexOf('<script src="/assets/theme.js">') < project.indexOf('<link rel="stylesheet"'));
  assert.doesNotMatch(project, /site\/public\/assets|Not an input/);
  assert.deepEqual(JSON.parse(readFileSync(join(dist, 'BUILD.json'))), { source_commit: COMMIT, version: '0.1.4', documentation_pages: 29, hosts: HOSTS });
  assert.deepEqual(readFileSync(join(dist, 'docs/_headers')), readFileSync(join(root, 'site/public/_headers')));
  assert.ok(existsSync(join(dist, 'landing/_redirects')));
  assert.equal(existsSync(join(dist, 'docs/_redirects')), false);
  assert.match(readFileSync(join(dist, 'docs/404.html'), 'utf8'), /<meta name="robots" content="noindex">/);
  const sums = readFileSync(join(dist, 'SITE-SHA256SUMS'), 'utf8');
  const entries = sums.trimEnd().split('\n').map(line => {
    assert.match(line, /^[a-f\d]{64}  [a-zA-Z\d_./-]+$/);
    const [hash, path] = line.split('  ');
    assert.equal(digest(readFileSync(join(dist, path))), hash, path);
    return path;
  });
  assert.deepEqual(entries, files(dist).filter(path => path !== 'SITE-SHA256SUMS'));
  for (const path of entries.filter(path => path.endsWith('.html'))) assert.match(readFileSync(join(dist, path), 'utf8'), new RegExp(`meta name="cqlmatic-source-commit" content="${COMMIT}"`));
  write(root, 'site/dist/stale.txt', 'obsolete');
  assert.deepEqual(buildSite({ root, commit: COMMIT }), first);
  assert.equal(readFileSync(join(dist, 'SITE-SHA256SUMS'), 'utf8'), sums);
  assert.equal(existsSync(join(dist, 'stale.txt')), false);
  assert.deepEqual(protectedPaths.map(path => digest(readFileSync(join(root, path)))), before);
});

test('invalid links fail before replacing the last successful bundle', async t => {
  const root = fixture(t);
  buildSite({ root, commit: COMMIT });
  const sums = readFileSync(join(root, 'site/dist/SITE-SHA256SUMS'), 'utf8');
  const cases = [
    ['[Missing](examples/missing.json)', /Missing/],
    ['[Missing anchor](docs/SETUP.md#absent)', /Missing anchor/],
    ['<a href="docs/SETUP.md#absent">Raw missing</a>', /Missing anchor/],
    ['[Unlisted](site/UNLISTED.md)', /Unapproved/],
    ['[Outside](../../outside.md)', /Unapproved/],
    ['[Encoded outside](%2e%2e/%2e%2e/outside.md)', /Unapproved/],
    ['[Unknown docs](https://docs.cqlmatic.dev/missing/)', /Missing site route/],
    ['[Bad source anchor](src/lib.rs#L999)', /missing source anchor/],
    ['![External](https://example.com/image.png)', /canonical local asset/],
    ['<img src="/assets/unapproved.png" alt="Missing">', /Unapproved canonical asset/],
    ['<a href="javascript:alert(1)">Unsafe</a>', /Unsupported link protocol/],
    ['<a href="#description">Metadata is not an anchor</a>', /Missing anchor/],
  ];
  for (const [markdown, error] of cases) await t.test(markdown, () => {
    write(root, 'README.md', `# Project\n\n${markdown}\n`);
    assert.throws(() => buildSite({ root, commit: COMMIT }), error);
    assert.equal(readFileSync(join(root, 'site/dist/SITE-SHA256SUMS'), 'utf8'), sums);
  });
});

test('symbolic-link inputs cannot escape the approved source files', t => {
  const root = fixture(t);
  rmSync(join(root, 'docs/SETUP.md'));
  symlinkSync(join(root, 'README.md'), join(root, 'docs/SETUP.md'));
  assert.throws(() => buildSite({ root, commit: COMMIT }), /symbolic-link input: docs\/SETUP.md/);
});

test('Wrangler configuration is pure Pages with separate output directories', () => {
  for (const [file, output] of [['wrangler.jsonc', './dist/landing'], ['wrangler.docs.jsonc', './dist/docs']]) {
    const config = JSON.parse(readFileSync(new URL(file, import.meta.url), 'utf8'));
    assert.equal(config.pages_build_output_dir, output);
    assert.deepEqual(Object.keys(config).sort(), ['$schema', 'name', 'pages_build_output_dir']);
  }
});
