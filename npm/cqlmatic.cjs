#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawn } = require('node:child_process');

function main() {
  if (Number(process.versions.node.split('.')[0]) < 22) {
    throw new Error('Node.js 22 or newer is required.');
  }
  const tuple = `${process.platform}-${process.arch}`;
  if (tuple !== 'darwin-arm64' && tuple !== 'linux-x64') {
    throw new Error(`Unsupported platform ${tuple}. Use a native release or build from source.`);
  }

  const root = path.resolve(__dirname, '..');
  const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, 'native-manifest.json'), 'utf8'));
  if (pkg.version !== '0.1.2' || manifest.version !== pkg.version) {
    throw new Error('Package and native manifest versions must both be 0.1.2.');
  }
  const digest = manifest.binaries?.[tuple]?.sha256;
  if (typeof digest !== 'string' || !/^[a-f0-9]{64}$/.test(digest)) {
    throw new Error('Invalid bundled executable digest. Use a native release or build from source.');
  }

  const native = path.join(root, 'native');
  const directory = path.join(native, tuple);
  const executable = path.join(directory, 'cqlmatic');
  if (!fs.lstatSync(native).isDirectory() || !fs.lstatSync(directory).isDirectory()) {
    throw new Error('Bundled executable directories must not be symbolic links.');
  }
  const stat = fs.lstatSync(executable);
  if (!stat.isFile() || (stat.mode & 0o111) === 0) {
    throw new Error('Bundled executable must be a regular, non-symlink executable file.');
  }
  const actual = createHash('sha256').update(fs.readFileSync(executable)).digest('hex');
  if (actual !== digest) {
    throw new Error('Bundled executable checksum does not match. Use a native release or build from source.');
  }

  const child = spawn(executable, process.argv.slice(2), { shell: false, stdio: 'inherit' });
  const handlers = new Map();
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    const forward = () => child.kill(signal);
    handlers.set(signal, forward);
    process.on(signal, forward);
  }
  function cleanup() {
    for (const [signal, handler] of handlers) process.removeListener(signal, handler);
  }
  child.once('error', (error) => {
    cleanup();
    process.stderr.write(`cqlmatic: could not start bundled executable (${error.code || 'unknown error'}).\n`);
    process.exitCode = 1;
  });
  child.once('close', (code, signal) => {
    cleanup();
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code === null || code < 0 ? 1 : code;
  });
}

try {
  main();
} catch (error) {
  const message = error.code
    ? `Could not read bundled installation (${error.code}). Use a native release or build from source.`
    : error instanceof SyntaxError ? 'Invalid package or native manifest JSON.' : error.message;
  process.stderr.write(`cqlmatic: ${message}\n`);
  process.exitCode = 1;
}
