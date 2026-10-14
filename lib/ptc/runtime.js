/** Runtime discovery is installation-relative; explicit selections never silently fall back. */
import { createHash } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
export const PACKAGE_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
export const PLATFORMS = Object.freeze(['win32-x64'])
export function workerName(platform = process.platform) { return platform === 'win32' ? 'dsh-ops-worker.exe' : 'dsh-ops-worker' }

/**
 * Validate one staged worker against the receipt beside it.
 *
 * The receipt is a staging fact — it records which target's build was copied
 * into which `runtime/<target>/` directory — so the directory name must match
 * `receipt.target` on every platform. `strict` additionally requires that
 * target to be the host's own, which is what automatic resolution promises; an
 * explicitly selected path is the operator's decision instead, so it only has
 * to be internally consistent and byte-exact.
 *
 * @param {string} file - the candidate worker.
 * @param {string} packageRoot - the package whose version the receipt records.
 * @param {string} target - this host's `${platform}-${arch}`.
 * @param {boolean} strict - whether the receipt must name this host's target.
 * @returns {string} the same path, once it has been verified.
 */
function verifiedWorker(file, packageRoot, target, strict) {
  if (!file.includes(`${path.sep}runtime${path.sep}`)) return file
  const staged = path.basename(path.dirname(file))
  let receipt, version
  try {
    receipt = JSON.parse(fs.readFileSync(path.join(path.dirname(file), 'provenance.json'), 'utf8'))
    version = JSON.parse(fs.readFileSync(path.join(packageRoot, 'package.json'), 'utf8')).version
  } catch (error) {
    throw new Error(`Native worker provenance is unavailable or unreadable (${error?.code ?? error?.name}).`)
  }
  const declared = typeof receipt.target === 'string' ? receipt.target : staged
  const platformMismatch = strict ? receipt.target !== target : receipt.target !== staged
  if (platformMismatch || receipt.version !== version || receipt.executable !== workerName(declared.split('-')[0])
    || receipt.sha256 !== createHash('sha256').update(fs.readFileSync(file)).digest('hex')) {
    throw new Error('Native worker provenance mismatch.')
  }
  return file
}

export function resolveWorker(config = {}, { packageRoot = PACKAGE_ROOT, env = process.env, platform = process.platform, arch = process.arch } = {}) {
  const target = `${platform}-${arch}`
  const explicit = config.binaryPath ?? env.DSH_OPS_WORKER_BIN
  if (explicit !== undefined) {
    // An explicit selection outranks the platform whitelist: it is the
    // operator's own build, so it is honored on every platform, including the
    // ones automatic resolution does not cover. It still never falls back —
    // an unusable configured path is reported instead of being skipped.
    if (!path.isAbsolute(explicit)) throw new Error('Native worker selection must be an absolute path.')
    let stat
    try { stat = fs.statSync(explicit) } catch { throw new Error(`Native worker unavailable at the configured path ${explicit}.`) }
    if (!stat.isFile()) throw new Error(`Native worker unavailable at the configured path ${explicit}.`)
    return verifiedWorker(explicit, packageRoot, target, false)
  }
  if (!PLATFORMS.includes(target)) throw new Error(`dsh-ops 0.3.0 supports Windows x64 only; unsupported runtime target ${target}. Set binaryPath or DSH_OPS_WORKER_BIN to an absolute worker path to run a worker built for this machine.`)
  const candidates = [
    path.join(packageRoot, 'runtime', target, workerName(platform)),
    path.join(packageRoot, 'native', 'target', 'release', workerName(platform)),
  ]
  for (const file of candidates) {
    if (!path.isAbsolute(file)) throw new Error('Native worker selection must be an absolute path.')
    let stat
    try { stat = fs.statSync(file) } catch { continue }
    if (!stat.isFile()) continue
    return verifiedWorker(file, packageRoot, target, true)
  }
  throw new Error(`Native worker unavailable for ${target}. Install a runtime-bearing dsh-ops package, or run npm run build:ptc in the source checkout. No ACL repair, shell fallback or automatic download is performed.`)
}
