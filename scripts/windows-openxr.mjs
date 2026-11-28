/** What a PC VR package adds beside a Windows game (docs/quest.md, "PC VR on Windows"): the OpenXR artifact
 * (`gpu/openxr`, `exact_openxr.dll`), the Khronos OpenXR loader (`openxr_loader.dll`) and a launcher that opens the
 * game in the headset. app.json `"host": {"windows": {"xr": "openxr"}}` asks for it; without it a Windows package
 * has none of them, and with it the game still opens flat unless launched with `--vr` (or `EXACT_XR=1`). */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { zipEntry } from './android-libraries.mjs';

/** The Khronos loader release for Windows the package carries: the same version as Android's (`OPENXR_LOADER`),
 * GitHub's `openxr_loader_windows-<version>.zip`, its x64 `openxr_loader.dll`. */
export const OPENXR_LOADER_WINDOWS = {
  version: '1.1.63',
  sha256: '01c631aeabbfe0879540f77ef833416c532a20746285b494630160c23588b771',
};

/** Whether app.json asks for the PC VR package. */
export const wantsPcVr = (manifest) => manifest?.host?.windows?.xr === 'openxr';

const digest = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');

/** Copies the pinned loader's x64 DLL into `output`, fetching its release zip once into the cache. Its path. */
export function stageOpenxrLoaderWindows(output, env = process.env) {
  const { version, sha256 } = OPENXR_LOADER_WINDOWS;
  const cache = resolve(env.XDG_CACHE_HOME ?? resolve(homedir(), '.cache'), 'exact', 'openxr-loader', version);
  const zip = resolve(cache, `openxr_loader_windows-${version}.zip`);
  if (!existsSync(zip) || digest(zip) !== sha256) {
    mkdirSync(cache, { recursive: true });
    const url = `https://github.com/KhronosGroup/OpenXR-SDK-Source/releases/download/release-${version}/openxr_loader_windows-${version}.zip`;
    const partial = `${zip}.partial`;
    const fetched = spawnSync('curl', ['-fsSL', '-o', partial, url], { stdio: 'inherit' });
    if (fetched.status !== 0) throw new Error(`the OpenXR loader did not download from ${url}`);
    if (digest(partial) !== sha256) { rmSync(partial, { force: true }); throw new Error(`the OpenXR loader from ${url} is not the pinned one (sha256 ${sha256})`); }
    renameSync(partial, zip);
  }
  const library = zipEntry(readFileSync(zip), 'x64/bin/openxr_loader.dll');
  if (!library) throw new Error(`the OpenXR loader ${version} has no x64/bin/openxr_loader.dll`);
  mkdirSync(output, { recursive: true });
  const path = resolve(output, 'openxr_loader.dll');
  writeFileSync(path, library);
  return path;
}

/** Builds the OpenXR artifact for `target` at `profile` in exact2's own workspace (it depends on no app), at
 * exact2's lock, into `targetDir`, and copies it into `output`. Its path. */
export function stageOpenxrArtifactWindows(output, { target, profile, env = process.env, targetDir }) {
  const built = spawnSync('cargo', ['build', '--locked', '-p', 'exact-openxr', '--target', target, '--profile', profile],
    { cwd: resolve(import.meta.dir, '..'), env: { ...env, CARGO_TARGET_DIR: targetDir }, stdio: 'inherit' });
  if (built.status !== 0) throw new Error('the OpenXR artifact (gpu/openxr) did not build');
  mkdirSync(output, { recursive: true });
  const path = resolve(output, 'exact_openxr.dll');
  copyFileSync(resolve(targetDir, target, profile, 'exact_openxr.dll'), path);
  return path;
}

/** The launcher beside `<exe>`: it opens the game in the headset (`--vr`) and waits for it, its standard error
 * kept in `xr\\launch.log` (the artifact writes `xr\\exact-xr.log` itself). Its path. */
export function writePcVrLauncher(output, exe) {
  const name = exe.replace(/\.exe$/i, '');
  const path = resolve(output, `${name} VR.cmd`);
  const commandExe = exe.replaceAll('%', '%%');
  writeFileSync(path, [
    '@echo off',
    'setlocal DisableDelayedExpansion',
    'cd /d "%~dp0"',
    'if not exist xr mkdir xr',
    'echo Opening the game in VR. Logs: "%~dp0xr\\exact-xr.log" and "%~dp0xr\\launch.log".',
    'echo Close the game window to stop.',
    `"%~dp0${commandExe}" --vr %* 2> "%~dp0xr\\launch.log"`,
    '',
  ].join('\r\n'));
  return path;
}
