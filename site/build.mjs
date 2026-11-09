import { createHash } from 'node:crypto';
import { lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Marked, Renderer, TextRenderer } from 'marked';
import GithubSlugger from 'github-slugger';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const REPOSITORY = 'https://github.com/mychaelconnolly/cqlmatic';
export const HOSTS = { landing: 'https://cqlmatic.dev', docs: 'https://docs.cqlmatic.dev' };

// These are the complete documentation inputs, independent of untracked Markdown.
export const DOCUMENTS = [
  ['docs/README.md', '/', 'Overview', 'Start here'],
  ['docs/SETUP.md', '/setup/', 'Setup', 'Start here'],
  ['docs/INVENTORY.md', '/inventory/', 'Inventory and status', 'Start here'],
  ['docs/FLOW.md', '/flow/', 'Application flow', 'Start here'],
  ['docs/ANALYST-WORKFLOW.md', '/analyst-workflow/', 'Analyst workflow', 'Start here'],
  ['USAGE.md', '/usage/', 'Command reference', 'Reference'],
  ['docs/QUERY-REFERENCE.md', '/query-reference/', 'Query reference', 'Reference'],
  ['docs/ENVIRONMENT.md', '/environment/', 'Environment packs', 'Reference'],
  ['docs/MCP-COMPANION.md', '/mcp-companion/', 'MCP companion', 'Reference'],
  ['docs/RESULT-ASSESSMENT.md', '/result-assessment/', 'Result assessment', 'Reference'],
  ['docs/CONTRACTS.md', '/contracts/', 'Contracts', 'Reference'],
  ['docs/COVERAGE.md', '/coverage/', 'Coverage and limits', 'Reference'],
  ['docs/TROUBLESHOOTING.md', '/troubleshooting/', 'Troubleshooting', 'Reference'],
  ['examples/setup/README.md', '/examples/setup/', 'Setup examples', 'Examples and integrations'],
  ['examples/inventory/README.md', '/examples/inventory/', 'Inventory examples', 'Examples and integrations'],
  ['examples/mcp/README.md', '/examples/mcp/', 'MCP examples', 'Examples and integrations'],
  ['examples/query-reference/README.md', '/examples/query-reference/', 'Query examples', 'Examples and integrations'],
  ['integrations/agent/AGENTS-snippet.md', '/integrations/agent/agents-snippet/', 'Agent instructions', 'Examples and integrations'],
  ['integrations/agent/SKILL.md', '/integrations/agent/skill/', 'Agent skill', 'Examples and integrations'],
  ['README.md', '/project/', 'Project README', 'Project'],
  ['docs/ARCHITECTURE.md', '/architecture/', 'Architecture', 'Project'],
  ['docs/DEVELOPMENT.md', '/development/', 'Development', 'Project'],
  ['docs/RELEASING.md', '/releasing/', 'Releasing', 'Project'],
  ['CONTRIBUTING.md', '/contributing/', 'Contributing', 'Project'],
  ['AGENTS.md', '/agents/', 'Repository agent guide', 'Project'],
  ['CHANGELOG.md', '/changelog/', 'Changelog', 'Project'],
  ['RELEASE-NOTES.md', '/release-notes/', 'Release notes', 'Project'],
  ['SECURITY.md', '/security/', 'Security', 'Project'],
  ['CODE_OF_CONDUCT.md', '/code_of_conduct/', 'Code of conduct', 'Project'],
].map(([source, route, label, group]) => ({ source, route, label, group }));

const ASSETS = new Set([
  'favicon.svg', 'theme.js', 'quick-start.js', 'site.css', 'docs.css', 'github-mark.svg', 'github-mark.LICENSE.txt',
  'agentworks-expert-builder-2026.png', 'setup-discovery-query.png',
]);
const SOURCE_ROOTS = ['src', 'data', 'schemas', 'examples', 'licenses', 'tests', 'npm', 'tools', '.github'];
const SOURCE_FILES = new Set([
  'LICENSE', 'NOTICE', 'Cargo.toml', 'Cargo.lock', 'dev', 'rust-toolchain.toml', 'package.json',
  'site/package.json', 'site/package-lock.json', 'site/build.mjs', 'site/build.test.mjs',
  'site/theme.test.cjs', 'site/wrangler.jsonc', 'site/wrangler.docs.jsonc',
]);
const HTML_TAG = /<[a-z](?:[^>"']|"[^"]*"|'[^']*')*>/gi;
const escape = value => String(value).replace(/[&<>"']/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);
const decode = value => value.replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi, (entity, code) => {
  if (code[0] !== '#') return ({ amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: '\u00a0' })[code.toLowerCase()];
  const point = code[1].toLowerCase() === 'x' ? parseInt(code.slice(2), 16) : Number(code.slice(1));
  return point > 0 && point <= 0x10ffff ? String.fromCodePoint(point) : entity;
});
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const github = (commit, path, directory = false) => `${REPOSITORY}/${directory ? 'tree' : 'blob'}/${commit}${path ? `/${path.split('/').map(encodeURIComponent).join('/')}` : ''}`;

export function requireCommit(commit) {
  if (typeof commit !== 'string' || !/^[a-f\d]{40}$/i.test(commit)) throw new Error('Provide --commit with a full 40-character hexadecimal source commit.');
  return commit.toLowerCase();
}

function inspect(root, source) {
  const path = join(root, source);
  let info;
  try {
    info = lstatSync(path);
    if (realpathSync(path) !== path) throw new Error('symbolic link');
  } catch {
    throw new Error(`Missing or symbolic-link input: ${source}`);
  }
  if (!info.isFile() && !info.isDirectory()) throw new Error(`Unsupported input: ${source}`);
  return info;
}

function read(root, source) {
  if (!inspect(root, source).isFile()) throw new Error(`Expected a file: ${source}`);
  return readFileSync(join(root, source));
}

export function documentRegistry(documents = DOCUMENTS) {
  const sources = new Map();
  const routes = new Map();
  for (const document of documents) {
    if (!DOCUMENTS.some(approved => approved.source === document.source && approved.route === document.route)) throw new Error(`Unapproved documentation input or route: ${document.source}`);
    if (sources.has(document.source) || routes.has(document.route)) throw new Error(`Duplicate documentation source or route: ${document.source}`);
    const copy = { ...document };
    sources.set(document.source, copy);
    routes.set(document.route, copy);
  }
  return { sources, routes };
}

export function renderMarkdown(source, markdown) {
  if (!DOCUMENTS.some(document => document.source === source)) throw new Error(`Unapproved documentation input: ${source}`);
  if (source === 'integrations/agent/SKILL.md' && markdown.startsWith('---\n')) {
    const end = markdown.indexOf('\n---\n', 4);
    if (end < 0) throw new Error(`Unclosed frontmatter: ${source}`);
    markdown = markdown.slice(end + 5);
  }
  const slugger = new GithubSlugger();
  const textRenderer = new TextRenderer();
  textRenderer.html = () => '';
  const renderer = new Renderer();
  const baseCode = renderer.code;
  const baseTable = renderer.table;
  const headings = [];
  renderer.heading = function ({ tokens, depth }) {
    textRenderer.parser = this.parser;
    const text = decode(this.parser.parseInline(tokens, textRenderer));
    const id = slugger.slug(text);
    headings.push({ text, id, depth });
    return `<h${depth} id="${escape(id)}">${this.parser.parseInline(tokens)}</h${depth}>\n`;
  };
  renderer.code = function (token) { return baseCode.call(this, token).replace('<pre>', '<pre tabindex="0">'); };
  renderer.table = function (token) { return `<div class="docs-table" role="region" aria-label="Reference table" tabindex="0">${baseTable.call(this, token)}</div>\n`; };
  const html = new Marked({ renderer, gfm: true }).parse(markdown);
  return { html, headings, title: headings.find(heading => heading.depth === 1)?.text || source, anchors: anchorsIn(html, source) };
}

function anchorsIn(html, source) {
  const anchors = new Set();
  for (const tag of html.match(HTML_TAG) || []) {
    for (const match of tag.matchAll(/(?<=\s)(id|name)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/gi)) {
      if (match[1].toLowerCase() === 'name' && !/^<a\b/i.test(tag)) continue;
      const anchor = decode(match[2] ?? match[3] ?? match[4]);
      if (anchors.has(anchor)) throw new Error(`Duplicate anchor in ${source}: ${anchor}`);
      anchors.add(anchor);
    }
  }
  return anchors;
}

function fragmentFor(document, fragment) {
  if (!fragment) return '';
  let anchor;
  try { anchor = decodeURIComponent(fragment); } catch { throw new Error(`Invalid fragment in ${document.source}: ${fragment}`); }
  if (!document.anchors.has(anchor)) throw new Error(`Missing anchor in ${document.source}: ${anchor}`);
  return `#${fragment}`;
}

function resolveLink(root, registry, landing, commit, current, value, kind, assets) {
  const href = decode(value).trim();
  if (!href || /[\u0000-\u001f\\]/.test(href)) throw new Error(`Invalid local link in ${current.source}`);
  if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href)) {
    const url = new URL(href, current.host);
    if (!['https:', 'http:', 'mailto:', 'tel:'].includes(url.protocol)) throw new Error(`Unsupported link protocol in ${current.source}`);
    if (url.origin === HOSTS.docs || url.origin === HOSTS.landing) {
      const target = url.origin === HOSTS.docs ? registry.routes.get(url.pathname) : landing.get(url.pathname);
      if (target) return `${url.origin}${target.route}${url.search}${fragmentFor(target, url.hash.slice(1))}`;
      if (url.pathname.startsWith('/assets/')) return resolveAsset(root, url.pathname.slice(8), assets);
      throw new Error(`Missing site route in ${current.source}: ${url.pathname}`);
    }
    const source = url.href.match(/^https:\/\/github\.com\/mychaelconnolly\/cqlmatic\/(blob|tree)\/[^/]+\/([^?#]+)(.*)$/);
    if (source) return resolveLink(root, registry, landing, commit, current, `/${source[2]}${source[3]}`, kind, assets);
    if (kind !== 'href') throw new Error(`Image must use a canonical local asset in ${current.source}`);
    return href;
  }
  const [, encodedPath, query = '', fragment = ''] = href.match(/^([^?#]*)(\?[^#]*)?(?:#(.*))?$/);
  if (!encodedPath) return `${query}${fragmentFor(current, fragment)}`;
  let path;
  try { path = decodeURIComponent(encodedPath); } catch { throw new Error(`Invalid path in ${current.source}`); }
  if (path.startsWith('/assets/')) return `${resolveAsset(root, path.slice(8), assets)}${query}${fragment ? `#${fragment}` : ''}`;
  if (path.startsWith('/')) {
    const target = current.host === HOSTS.docs ? registry.routes.get(path) : landing.get(path);
    if (target) return `${target.route}${query}${fragmentFor(target, fragment)}`;
  }
  const source = posix.normalize(path.startsWith('/') ? path.slice(1) : posix.join(posix.dirname(current.source), path)).replace(/\/$/, '') || '.';
  if (source === '..' || source.startsWith('../') || source.split('/').some(part => part.startsWith('.') && part !== '.github')) throw new Error(`Unapproved local target in ${current.source}: ${source}`);
  const target = registry.sources.get(source);
  if (target) return `${current.host === HOSTS.docs ? '' : HOSTS.docs}${target.route}${query}${fragmentFor(target, fragment)}`;
  if (source.startsWith('site/public/assets/')) return `${resolveAsset(root, source.slice(19), assets)}${query}${fragment ? `#${fragment}` : ''}`;
  if (!SOURCE_FILES.has(source) && !SOURCE_ROOTS.some(prefix => source === prefix || source.startsWith(`${prefix}/`))) throw new Error(`Unapproved local target in ${current.source}: ${source}`);
  if (source.endsWith('.md')) throw new Error(`Unapproved Markdown target in ${current.source}: ${source}`);
  const info = inspect(root, source);
  if (kind !== 'href') throw new Error(`Image must use a canonical local asset in ${current.source}`);
  if (fragment) {
    const lines = fragment.match(/^L([1-9]\d*)(?:-L([1-9]\d*))?$/);
    const count = info.isFile() ? read(root, source).toString('utf8').split('\n').length : 0;
    if (!lines || Number(lines[1]) > count || (lines[2] && (Number(lines[2]) < Number(lines[1]) || Number(lines[2]) > count))) throw new Error(`Unsupported or missing source anchor in ${source}: ${fragment}`);
  }
  return `${github(commit, source, info.isDirectory())}${query}${fragment ? `#${fragment}` : ''}`;
}

function resolveAsset(root, name, assets) {
  if (!ASSETS.has(name)) throw new Error(`Unapproved canonical asset: ${name}`);
  read(root, `site/public/assets/${name}`);
  assets.add(name);
  return `/assets/${name}`;
}

function rewriteHtml(html, resolver) {
  return html.replace(HTML_TAG, tag => tag.replace(/(?<=\s)(href|src|srcset)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))/gi, (attribute, kind, double, single, bare) => {
    const value = double ?? single ?? bare;
    const rebased = kind.toLowerCase() === 'srcset'
      ? value.split(',').map(candidate => {
        const [url, ...descriptor] = candidate.trim().split(/\s+/);
        return [resolver(url, 'src'), ...descriptor].join(' ');
      }).join(', ')
      : resolver(value, kind.toLowerCase());
    return `${kind}="${escape(rebased)}"`;
  }));
}

function navigation(current) {
  const groups = [...new Set(DOCUMENTS.map(document => document.group))];
  return `<nav aria-label="Documentation">${groups.map(group => `<section class="docs-nav-group"><h2>${group}</h2><ul>${DOCUMENTS.filter(document => document.group === group).map(document => `<li><a href="${document.route}"${document.source === current ? ' aria-current="page"' : ''}>${escape(document.label)}</a></li>`).join('')}</ul></section>`).join('')}</nav>`;
}

function shell({ title, description, route, content, navigation: nav = '', footer = '', notFound = false }) {
  const canonical = `${HOSTS.docs}${route}`;
  return `<!doctype html>
<html lang="en" data-theme="dark" data-theme-preference="dark">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${escape(title)} | CQLmatic documentation</title>
  <meta name="description" content="${escape(description)}">
  <meta name="theme-color" content="#151517">
  ${notFound ? '<meta name="robots" content="noindex">' : `<link rel="canonical" href="${canonical}">`}
  <meta property="og:type" content="website">
  <meta property="og:site_name" content="CQLmatic documentation">
  <meta property="og:title" content="${escape(title)} | CQLmatic documentation">
  <meta property="og:description" content="${escape(description)}">
  <meta property="og:url" content="${canonical}">
  <meta name="twitter:card" content="summary">
  <meta name="twitter:title" content="${escape(title)} | CQLmatic documentation">
  <meta name="twitter:description" content="${escape(description)}">
  <link rel="icon" href="/assets/favicon.svg" type="image/svg+xml">
  <script src="/assets/theme.js"></script>
  <link rel="stylesheet" href="/assets/site.css">
  <link rel="stylesheet" href="/assets/docs.css">
</head>
<body class="docs-site">
  <a class="skip-link" href="#main">Skip to content</a>
  <header class="site-header wrap">
    <a class="wordmark" href="${HOSTS.landing}/" aria-label="CQLmatic home">CQL<span>matic</span></a>
    <nav aria-label="Main navigation"><a href="/">Documentation</a></nav>
    <button class="theme-toggle" type="button" hidden aria-label="Theme: dark. Switch to light." title="Theme: dark. Switch to light.">
      <svg data-theme-icon="dark" aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M20.5 14A8.5 8.5 0 0 1 10 3.5 8.5 8.5 0 1 0 20.5 14Z"/></svg>
      <svg data-theme-icon="light" hidden aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5"/></svg>
      <svg data-theme-icon="auto" hidden aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 1 0 16Z" fill="currentColor" stroke="none"/></svg>
    </button>
    <a class="github-link" href="${REPOSITORY}" aria-label="CQLmatic on GitHub" title="CQLmatic on GitHub"><img src="/assets/github-mark.svg" width="24" height="24" alt=""></a>
  </header>
  ${nav ? `<div class="docs-mobile-nav wrap"><details><summary>Browse documentation</summary>${nav}</details></div>` : ''}
  <div class="docs-layout wrap">
    ${nav ? `<aside class="docs-sidebar">${nav}</aside>` : ''}
    <main id="main" class="docs-main" tabindex="-1">${content}${footer}</main>
  </div>
  <footer class="site-footer wrap"><p>CQLmatic <span>v0.1.1 documentation</span></p><p><a href="${HOSTS.landing}/">Project home</a> · <a href="${REPOSITORY}">GitHub</a></p></footer>
</body>
</html>
`;
}

export function buildSite({ root = ROOT, commit } = {}) {
  commit = requireCommit(commit);
  root = realpathSync(root);
  const registry = documentRegistry();
  for (const document of registry.sources.values()) Object.assign(document, renderMarkdown(document.source, read(root, document.source).toString('utf8')), { host: HOSTS.docs });
  const landing = new Map([['/', { source: 'site/public/index.html', route: '/' }], ['/404.html', { source: 'site/public/404.html', route: '/404.html' }]]);
  for (const document of landing.values()) {
    document.html = read(root, document.source).toString('utf8');
    document.anchors = anchorsIn(document.html, document.source);
    document.host = HOSTS.landing;
  }
  const output = new Map();
  const stamp = html => html.replace('<head>', `<head>\n  <meta name="cqlmatic-source-commit" content="${commit}">`);
  for (const host of ['landing', 'docs']) {
    const assets = new Set(['github-mark.svg', 'github-mark.LICENSE.txt', ...(host === 'docs' ? ['favicon.svg', 'theme.js', 'site.css', 'docs.css'] : [])]);
    const documents = host === 'landing' ? landing.values() : registry.sources.values();
    for (const document of documents) {
      const content = rewriteHtml(document.html, (href, kind) => resolveLink(root, registry, landing, commit, document, href, kind, assets));
      const html = host === 'landing' ? content : shell({
        title: document.title, description: `${document.label} for CQLmatic 0.1.1.`, route: document.route,
        navigation: navigation(document.source), content: `<article class="docs-article">${content}</article>`,
        footer: `<p class="docs-source"><a href="${github(commit, document.source)}">View Markdown source</a> · <a href="${github(commit, '', true)}">Source ${commit.slice(0, 12)}</a></p>`,
      });
      output.set(`${host}/${document.route === '/404.html' ? '404.html' : `${document.route.slice(1)}index.html`}`, Buffer.from(stamp(html)));
    }
    for (const asset of assets) output.set(`${host}/assets/${asset}`, read(root, `site/public/assets/${asset}`));
    output.set(`${host}/_headers`, read(root, 'site/public/_headers'));
  }
  output.set('landing/_redirects', read(root, 'site/public/_redirects'));
  output.set('docs/404.html', Buffer.from(stamp(shell({ title: 'Page not found', description: 'This documentation page could not be found. Return to the CQLmatic documentation index.', route: '/404.html', notFound: true, content: '<article class="docs-article docs-not-found"><h1>That page isn’t here.</h1><p>The address may have changed or the page may not exist.</p><p><a href="/">Return to the documentation index</a></p></article>' }))));
  output.set('BUILD.json', Buffer.from(`${JSON.stringify({ source_commit: commit, version: '0.1.1', documentation_pages: DOCUMENTS.length, hosts: HOSTS }, null, 2)}\n`));
  const sums = [...output.keys()].sort().map(path => `${sha256(output.get(path))}  ${path}\n`).join('');
  output.set('SITE-SHA256SUMS', Buffer.from(sums));
  const dist = join(root, 'site/dist');
  rmSync(dist, { recursive: true, force: true });
  for (const path of [...output.keys()].sort()) {
    const target = join(dist, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, output.get(path));
  }
  return { documentationPages: DOCUMENTS.length, files: output.size, sourceCommit: commit };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 4 || process.argv[2] !== '--commit') throw new Error('Usage: npm --prefix site run build -- --commit FULL40HEXSHA');
    const result = buildSite({ commit: process.argv[3] });
    console.log(`Built ${result.documentationPages} documentation pages and both static sites for ${result.sourceCommit}.`);
  } catch (error) {
    console.error(`Site build failed: ${error.message.replaceAll(ROOT, '[project]')}`);
    process.exitCode = 1;
  }
}
