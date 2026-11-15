// A tool set is taken by its inputs' bytes, and a fetched one only where it may run (LLP 1174 D3, D5).
import { test } from 'bun:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, resolve } from 'node:path';
import { cargoOnPath } from './cargo-check.mjs';
import { digest, lockClosure, workspaceManifest } from './kept.mjs';
import { ROOT, RustMissing, fromSet, fromSets, hostTriple, rustMissing } from './tools.mjs';

const pin = Bun.TOML.parse(readFileSync(resolve(ROOT, 'rust-toolchain.toml'), 'utf8')).toolchain.channel;
const sha = (text) => createHash('sha256').update(text).digest('hex');
const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
const triples = {
  'win32-x64': 'x86_64-pc-windows-msvc', 'win32-arm64': 'aarch64-pc-windows-msvc',
  'darwin-x64': 'x86_64-apple-darwin', 'darwin-arm64': 'aarch64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu', 'linux-arm64': 'aarch64-unknown-linux-gnu',
};
const triple = triples[`${process.platform}-${process.arch}`];

/** A machine: a HOME whose cache holds the sets `sets` describes, and whether it has Rust (a `cargo` on PATH and the
 * pinned toolchain). */
function machine(sets, { rust }) {
  const home = mkdtempSync(resolve(tmpdir(), 'exact-tools-')), bin = resolve(home, 'bin');
  mkdirSync(bin);
  if (rust) {
    writeFileSync(resolve(bin, cargo), '');
    mkdirSync(resolve(home, '.rustup/toolchains', `${pin}-${triple}`), { recursive: true });
  }
  sets.forEach(({ source, inputs, commit }, i) => {
    const dir = resolve(home, '.cache/exact/tools', triple, `set${i}`);
    mkdirSync(dir, { recursive: true });
    writeFileSync(resolve(dir, 'contract'), '');
    const product = { file: 'contract', crate: 'contract', inputs, closure: lockClosure(ROOT, 'contract'), workspace: sha(JSON.stringify(workspaceManifest(ROOT))) };
    writeFileSync(resolve(dir, 'manifest.json'), JSON.stringify({ version: 1, triple, commit, source, built: i, products: { contract: product } }));
  });
  const env = { HOME: home, PATH: bin, CARGO_HOME: resolve(home, '.cargo'), RUSTUP_HOME: resolve(home, '.rustup') };
  return { env, done: () => rmSync(home, { recursive: true, force: true }) };
}

const ours = ['rust-toolchain.toml', 'contract/syntax/src'].map(file => [file, digest(resolve(ROOT, file))]);

test('host triples use Rust target names for each supported machine', () => {
  for (const [machine, expected] of Object.entries(triples)) assert.equal(hostTriple(...machine.split('-')), expected);
  assert.equal(hostTriple('freebsd', 'x64'), 'x64-freebsd');
});

test('Rust detection finds Cargo on PATH or in its home and requires the pinned toolchain', () => {
  const m = machine([], { rust: true });
  try {
    assert.equal(hostTriple(), triple);
    assert.equal(cargoOnPath(m.env, m.env.HOME), true);
    assert.equal(rustMissing(m.env), false);
    const bin = m.env.PATH;
    m.env.PATH = resolve(m.env.HOME, 'empty');
    assert.equal(rustMissing(m.env), true);
    mkdirSync(m.env.CARGO_HOME);
    const rustupBin = resolve(m.env.CARGO_HOME, 'bin');
    renameSync(bin, rustupBin);
    const before = m.env.PATH;
    assert.equal(rustMissing(m.env), false);
    assert.equal(m.env.PATH, before);
    assert.equal(cargoOnPath(m.env, m.env.HOME), true);
    assert.equal(m.env.PATH, [rustupBin, before].join(delimiter));
    assert.equal(cargoOnPath(m.env, m.env.HOME), true);
    assert.equal(m.env.PATH, [rustupBin, before].join(delimiter));
    assert.equal(rustMissing({ ...m.env, RUSTUP_HOME: resolve(m.env.HOME, 'missing') }), true);
    assert.equal(rustMissing({ ...m.env, PATH: before, CARGO_HOME: resolve(m.env.HOME, 'missing') }), true);
    assert.equal(rustMissing({ HOME: m.env.HOME }), false);
    const custom = resolve(m.env.HOME, 'custom cargo');
    renameSync(m.env.CARGO_HOME, custom);
    assert.equal(rustMissing({ ...m.env, PATH: before, CARGO_HOME: custom }), false);
    assert.equal(rustMissing({ HOME: m.env.HOME }), true);
  } finally { m.done(); }
});

test('a set is taken while its inputs are these bytes, and a miss names the first that differs', () => {
  const changed = [ours[0], ['contract/syntax/src', 'f'.repeat(64)]];
  const m = machine([{ source: 'built', inputs: changed, commit: 'a'.repeat(40) }, { source: 'built', inputs: ours, commit: 'b'.repeat(40) }], { rust: false });
  try {
    assert.equal(fromSet('contract', 'built', m.env)?.path, resolve(m.env.HOME, '.cache/exact/tools', hostTriple(), 'set1', 'contract'));
  } finally { m.done(); }
  const miss = machine([{ source: 'built', inputs: changed, commit: 'c'.repeat(40) }], { rust: false });
  try {
    assert.equal(fromSet('contract', 'built', miss.env), null);
    assert.match(new RustMissing('contract', miss.env).message, /^error \[rust-missing\]: contract builds with Rust, .* contract\/syntax\/src differs from the newest tool set's, built at cccccccccccc.*https:\/\/rustup\.rs/);
  } finally { miss.done(); }
}, 60000);

test('a fetched set runs only without Rust or when an origin is named, and never under EXACT_TOOLS_ORIGIN=none', () => {
  for (const [rust, origin, taken] of [[true, undefined, false], [true, 'https://tools.example', true], [false, undefined, true], [false, 'none', false]]) {
    const m = machine([{ source: 'fetched', inputs: ours, commit: 'd'.repeat(40) }], { rust });
    try {
      const env = origin ? { ...m.env, EXACT_TOOLS_ORIGIN: origin } : m.env;
      assert.equal(!!fromSets('contract', env), taken, `rust ${rust}, origin ${origin}`);
    } finally { m.done(); }
  }
}, 60000);
