'use strict';

const assert = require('node:assert/strict');
const { test } = require('node:test');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawn, spawnSync } = require('node:child_process');

const tuple = `${process.platform}-${process.arch}`;
const supported = tuple === 'darwin-arm64' || tuple === 'linux-x64';
const launcherSource = fs.readFileSync(path.join(__dirname, 'cqlmatic.cjs'));

function fixture(t, body = 'process.exit(0);') {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cqlmatic-launcher-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'npm'));
  fs.mkdirSync(path.join(root, 'native', tuple), { recursive: true });
  const launcher = path.join(root, 'npm', 'cqlmatic.cjs');
  const binary = path.join(root, 'native', tuple, 'cqlmatic');
  const manifestPath = path.join(root, 'npm', 'native-manifest.json');
  const packagePath = path.join(root, 'package.json');
  const executable = `#!${process.execPath}\n${body}\n`;
  const manifest = {
    version: '0.1.2',
    binaries: { [tuple]: { sha256: createHash('sha256').update(executable).digest('hex') } },
  };
  fs.writeFileSync(launcher, launcherSource);
  fs.writeFileSync(binary, executable, { mode: 0o755 });
  fs.writeFileSync(packagePath, JSON.stringify({ version: '0.1.2' }));
  fs.writeFileSync(manifestPath, JSON.stringify(manifest));
  return { root, launcher, binary, manifestPath, packagePath, manifest };
}

function run(f, args = [], options = {}) {
  return spawnSync(process.execPath, [f.launcher, ...args], {
    encoding: 'utf8', timeout: 5000, ...options,
  });
}

function rejects(f, pattern) {
  const result = run(f);
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, pattern);
}

test('passes exact arguments, cwd, environment, stdin and structured output', { skip: !supported }, (t) => {
  const f = fixture(t, `
const fs = require('node:fs');
process.stdout.write(JSON.stringify({ args: process.argv.slice(2), cwd: process.cwd(),
  marker: process.env.CQLMATIC_TEST_MARKER, stdin: fs.readFileSync(0, 'utf8') }) + '\\n');
process.stderr.write('native diagnostic\\n');
`);
  const args = ['setup', '--json', 'space value', '$(echo unwanted)', '; exit 9', '', '*'];
  const result = run(f, args, {
    cwd: f.root, input: 'input\n', env: { ...process.env, CQLMATIC_TEST_MARKER: 'inherited' },
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0);
  assert.deepEqual(JSON.parse(result.stdout), {
    args, cwd: fs.realpathSync(f.root), marker: 'inherited', stdin: 'input\n',
  });
  assert.equal(result.stderr, 'native diagnostic\n');
});

test('mirrors a native nonzero exit code', { skip: !supported }, (t) => {
  const result = run(fixture(t, 'process.exit(37);'));
  assert.ifError(result.error);
  assert.equal(result.status, 37);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, '');
});

test('rejects missing, malformed and mismatched version metadata', { skip: !supported }, async (t) => {
  for (const [label, change, pattern] of [
    ['missing package', (f) => fs.unlinkSync(f.packagePath), /ENOENT/],
    ['missing manifest', (f) => fs.unlinkSync(f.manifestPath), /ENOENT/],
    ['malformed manifest', (f) => fs.writeFileSync(f.manifestPath, '{'), /Invalid.*JSON/],
    ['malformed package', (f) => fs.writeFileSync(f.packagePath, '{'), /Invalid.*JSON/],
    ['missing version', (f) => fs.writeFileSync(f.packagePath, '{}'), /versions/],
    ['wrong package version', (f) => fs.writeFileSync(f.packagePath, '{"version":"0.2.0"}'), /versions/],
    ['wrong manifest version', (f) => {
      f.manifest.version = '0.2.0';
      fs.writeFileSync(f.manifestPath, JSON.stringify(f.manifest));
    }, /versions/],
  ]) {
    await t.test(label, (t) => {
      const f = fixture(t);
      change(f);
      rejects(f, pattern);
    });
  }
});

test('rejects absent, malformed or mismatched SHA-256', { skip: !supported }, async (t) => {
  for (const digest of [undefined, 'PENDING', 'a'.repeat(63), 'A'.repeat(64), '0'.repeat(64)]) {
    await t.test(String(digest), (t) => {
      const f = fixture(t);
      f.manifest.binaries[tuple].sha256 = digest;
      fs.writeFileSync(f.manifestPath, JSON.stringify(f.manifest));
      rejects(f, /digest|checksum/);
    });
  }
});

test('rejects missing binary, non-executable file, directories and symlinks', { skip: !supported }, async (t) => {
  for (const [label, change, pattern] of [
    ['missing binary', (f) => fs.unlinkSync(f.binary), /ENOENT/],
    ['no execute bit', (f) => fs.chmodSync(f.binary, 0o644), /executable file/],
    ['directory', (f) => { fs.unlinkSync(f.binary); fs.mkdirSync(f.binary); }, /regular/],
    ['binary symlink', (f) => {
      fs.renameSync(f.binary, `${f.binary}.real`);
      fs.symlinkSync(`${f.binary}.real`, f.binary);
    }, /non-symlink/],
    ['tuple directory symlink', (f) => {
      const directory = path.dirname(f.binary);
      fs.renameSync(directory, `${directory}.real`);
      fs.symlinkSync(`${directory}.real`, directory);
    }, /symbolic links/],
    ['native directory symlink', (f) => {
      const directory = path.join(f.root, 'native');
      fs.renameSync(directory, `${directory}.real`);
      fs.symlinkSync(`${directory}.real`, directory);
    }, /symbolic links/],
  ]) {
    await t.test(label, (t) => {
      const f = fixture(t);
      change(f);
      rejects(f, pattern);
    });
  }
});

test('reports a spawn failure without leaking an executable path', { skip: !supported }, (t) => {
  const f = fixture(t);
  const executable = '#!/cqlmatic-missing-interpreter\n';
  fs.writeFileSync(f.binary, executable);
  f.manifest.binaries[tuple].sha256 = createHash('sha256').update(executable).digest('hex');
  fs.writeFileSync(f.manifestPath, JSON.stringify(f.manifest));
  rejects(f, /could not start bundled executable \(ENOENT\)/);
  assert.ok(!run(f).stderr.includes(f.root));
});

test('rejects unsupported platform combinations before launching', (t) => {
  const f = fixture(t);
  for (const [platform, arch] of [['linux', 'arm64'], ['darwin', 'x64'], ['win32', 'x64']]) {
    const script = `Object.defineProperty(process, 'platform', { value: ${JSON.stringify(platform)} });
Object.defineProperty(process, 'arch', { value: ${JSON.stringify(arch)} });
require(${JSON.stringify(f.launcher)});`;
    const result = spawnSync(process.execPath, ['-e', script], { encoding: 'utf8', timeout: 5000 });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /Unsupported platform.*native release or build from source/);
  }
});

test('rejects Node versions older than 22', (t) => {
  const f = fixture(t);
  const script = `Object.defineProperty(process.versions, 'node', { value: '20.0.0' });
require(${JSON.stringify(f.launcher)});`;
  const result = spawnSync(process.execPath, ['-e', script], { encoding: 'utf8', timeout: 5000 });
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /Node.js 22 or newer/);
});

test('mirrors termination by a native signal', { skip: !supported }, (t) => {
  const result = run(fixture(t, "process.kill(process.pid, 'SIGTERM');"));
  assert.ifError(result.error);
  assert.equal(result.status, null);
  assert.equal(result.signal, 'SIGTERM');
  assert.equal(result.stderr, '');
});

test('forwards termination signals and waits for native cleanup', { skip: !supported, timeout: 10000 }, async (t) => {
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    await t.test(signal, async (t) => {
      const f = fixture(t, `
process.on('${signal}', () => setTimeout(() => {
  process.stdout.write('cleaned\\n'); process.exit(23);
}, 50));
process.stdout.write('ready\\n');
setInterval(() => {}, 1000);
`);
      const child = spawn(process.execPath, [f.launcher], { stdio: ['ignore', 'pipe', 'pipe'] });
      t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); });
      let stdout = '';
      let stderr = '';
      let sent = false;
      child.stderr.on('data', (data) => { stderr += data; });
      child.stdout.on('data', (data) => {
        stdout += data;
        if (!sent && stdout.includes('ready\n')) {
          sent = true;
          child.kill(signal);
        }
      });
      const result = await new Promise((resolve, reject) => {
        child.once('error', reject);
        child.once('close', (code, signal) => resolve({ code, signal }));
      });
      assert.deepEqual(result, { code: 23, signal: null });
      assert.equal(stdout, 'ready\ncleaned\n');
      assert.equal(stderr, '');
    });
  }
});
