/** What the Android build reads in the libraries it packages: ELF images and alignments, a zip archive's entries,
 * and the OpenXR loader a headset app packages (docs/quest.md). */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { inflateRawSync } from 'node:zlib';

/** The SHA-256 of what an ELF64 file loads: its PT_LOAD segments' bytes, past the ELF header. AGP strips a
 * packaged library's debug symbols, which rewrites the header and nothing a segment holds, so this is the same
 * for the library a build staged and the one its APK holds, and differs for another build's (LLP 1159.002 D2). */
export function loadedImage(elf) {
  const headers = Number(elf.readBigUInt64LE(0x20)), size = elf.readUInt16LE(0x36), hash = createHash('sha256');
  for (let i = 0, count = elf.readUInt16LE(0x38); i < count; i++) {
    const at = headers + i * size;
    if (elf.readUInt32LE(at) !== 1) continue;
    const offset = Number(elf.readBigUInt64LE(at + 0x08));
    hash.update(elf.subarray(Math.max(offset, 64), offset + Number(elf.readBigUInt64LE(at + 0x20))));
  }
  return hash.digest('hex');
}

/** One file of a zip archive (an APK), stored or deflated, or null when the archive has none by that name. Read
 * here, with no `unzip`, so the build does the same on a Windows host. */
export function zipEntry(zip, name) {
  let end = zip.length - 22;
  while (end >= 0 && zip.readUInt32LE(end) !== 0x06054b50) end--;
  if (end < 0) return null;
  for (let at = zip.readUInt32LE(end + 16), left = zip.readUInt16LE(end + 10); left > 0 && zip.readUInt32LE(at) === 0x02014b50; left--) {
    const length = zip.readUInt16LE(at + 28);
    if (zip.toString('utf8', at + 46, at + 46 + length) === name) {
      const local = zip.readUInt32LE(at + 42), data = local + 30 + zip.readUInt16LE(local + 26) + zip.readUInt16LE(local + 28);
      const bytes = zip.subarray(data, data + zip.readUInt32LE(at + 20));
      return zip.readUInt16LE(at + 10) === 0 ? bytes : inflateRawSync(bytes);
    }
    at += 46 + length + zip.readUInt16LE(at + 30) + zip.readUInt16LE(at + 32);
  }
  return null;
}

/** Why an APK's library is not the one its build staged, or null (LLP 1159.002 D2): Gradle can keep an earlier
 * build's library in its merged intermediates and report the task up to date (QUEUE, "Gradle's file watching"). */
export function keptLibrary(staged, packaged, dirs, name = "libexactcanvas.so") {
  const made = loadedImage(staged), held = packaged ? loadedImage(packaged) : null;
  if (made === held) return null;
  return `the APK holds ${held ? `a ${name} that is not the one this build made (${held.slice(0, 12)} in the APK, ${made.slice(0, 12)} staged, by what each loads)` : `no ${name}`}. Gradle kept an earlier build's: delete ${dirs.join(' and ')} and build again`;
}

/** The smallest alignment of an ELF64 file's PT_LOAD segments (its program headers' p_align). */
export function loadAlignment(path) {
  const elf = readFileSync(path), headers = Number(elf.readBigUInt64LE(0x20)), size = elf.readUInt16LE(0x36);
  let least = Infinity;
  for (let i = 0, count = elf.readUInt16LE(0x38); i < count; i++) {
    if (elf.readUInt32LE(headers + i * size) === 1) least = Math.min(least, Number(elf.readBigUInt64LE(headers + i * size + 0x30)));
  }
  return least;
}

/** The Khronos OpenXR loader a headset app (app.json `host.android.xr`, docs/quest.md) packages beside its GPU module,
 * which opens it at run time: Maven Central's AAR at a pinned version and digest, cached under ~/.cache/exact. */
export const OPENXR_LOADER = {
  version: '1.1.63',
  sha256: '622419d2f6741c3443a3beb4779af0764318edd01830de967f24c741ebcded73',
};

/** Copies the OpenXR loader for `abi` into `jniLibs`, fetching the pinned AAR once. */
export function stageOpenxrLoader(jniLibs, abi, env = process.env) {
  const { version, sha256 } = OPENXR_LOADER;
  const cache = resolve(env.XDG_CACHE_HOME ?? resolve(homedir(), '.cache'), 'exact', 'openxr-loader', version);
  const aar = resolve(cache, `openxr_loader_for_android-${version}.aar`);
  const digest = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
  if (!existsSync(aar) || digest(aar) !== sha256) {
    mkdirSync(cache, { recursive: true });
    const url = `https://repo1.maven.org/maven2/org/khronos/openxr/openxr_loader_for_android/${version}/openxr_loader_for_android-${version}.aar`;
    const partial = `${aar}.partial`;
    const fetched = spawnSync('curl', ['-fsSL', '-o', partial, url], { stdio: 'inherit' });
    if (fetched.status !== 0) throw new Error(`the OpenXR loader did not download from ${url}`);
    if (digest(partial) !== sha256) { rmSync(partial, { force: true }); throw new Error(`the OpenXR loader from ${url} is not the pinned one (sha256 ${sha256})`); }
    renameSync(partial, aar);
  }
  const library = zipEntry(readFileSync(aar), `jni/${abi}/libopenxr_loader.so`);
  if (!library) throw new Error(`the OpenXR loader ${version} has no ${abi} library`);
  mkdirSync(jniLibs, { recursive: true });
  writeFileSync(resolve(jniLibs, 'libopenxr_loader.so'), library);
}
