/**
 * Platform selection contract: automatic resolution stays limited to the
 * platforms the package ships a runtime for, while an explicitly selected
 * worker — `config.binaryPath` or `DSH_OPS_WORKER_BIN` — is honored on every
 * platform, including the ones Node reports with a platform string of their
 * own (HarmonyOS reports `openharmony` and runs the Linux ABI worker).
 *
 * No worker binary is needed here: every case either fails before a spawn or
 * only reads a staged file, so this suite answers on any host.
 */
import fs from 'node:fs'
import { createHash } from 'node:crypto'
import path from 'node:path'
import { assert, PACKAGE_ROOT, test, report, withTempDir } from './lib/harness.mjs'
import { resolveWorker, workerName } from '../lib/ptc/runtime.js'

const version = JSON.parse(fs.readFileSync(path.join(PACKAGE_ROOT, 'package.json'), 'utf8')).version

/**
 * Build a throwaway package with one staged runtime target.
 * @param {string} root - the package directory to create.
 * @param {object} options - the staging to write.
 * @param {string} options.target - the `runtime/<target>/` directory name.
 * @param {string} [options.receiptTarget] - a receipt target that disagrees with the directory.
 * @param {boolean} [options.tamper] - change the bytes after the receipt was written.
 * @param {string} [options.receiptVersion] - a receipt version that disagrees with the package.
 * @returns {string} the staged worker path.
 */
function stage(root, { target, receiptTarget, tamper = false, receiptVersion = version }) {
  const executable = workerName(target.split('-')[0])
  const directory = path.join(root, 'runtime', target)
  fs.mkdirSync(directory, { recursive: true })
  const file = path.join(directory, executable)
  fs.writeFileSync(file, 'worker-bytes')
  fs.writeFileSync(path.join(directory, 'provenance.json'), `${JSON.stringify({
    version: receiptVersion, target: receiptTarget ?? target, executable, sha256: createHash('sha256').update('worker-bytes').digest('hex'),
  }, null, 2)}\n`)
  if (tamper) fs.writeFileSync(file, 'tampered-bytes')
  fs.writeFileSync(path.join(root, 'package.json'), `${JSON.stringify({ name: 'staged-fixture', version })}\n`)
  return file
}

await test('automatic resolution refuses a target the package does not ship', async () => {
  for (const [platform, arch] of [['linux', 'x64'], ['linux', 'arm64'], ['openharmony', 'arm64'], ['win32', 'arm64'], ['darwin', 'arm64']]) {
    assert.throws(() => resolveWorker({}, { platform, arch }), /Windows x64 only/, `${platform}-${arch} must stay gated`)
  }
})

await test('the refusal names the override that lifts it for a self-built worker', async () => {
  assert.throws(() => resolveWorker({}, { platform: 'openharmony', arch: 'arm64' }), /binaryPath or DSH_OPS_WORKER_BIN/)
})

await test('an explicit worker outside runtime/ is honored where automatic resolution refuses', async () => {
  await withTempDir('ops-platform-explicit', async dir => {
    const file = path.join(dir, 'self-built-worker')
    fs.writeFileSync(file, 'worker-bytes')
    assert.equal(resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), file)
    assert.equal(resolveWorker({}, { platform: 'openharmony', arch: 'arm64', env: { DSH_OPS_WORKER_BIN: file } }), file)
  })
})

await test('config.binaryPath outranks the environment selection', async () => {
  await withTempDir('ops-platform-precedence', async dir => {
    const configured = path.join(dir, 'configured-worker')
    const ambient = path.join(dir, 'ambient-worker')
    fs.writeFileSync(configured, 'worker-bytes')
    fs.writeFileSync(ambient, 'worker-bytes')
    assert.equal(resolveWorker({ binaryPath: configured }, { platform: 'openharmony', arch: 'arm64', env: { DSH_OPS_WORKER_BIN: ambient } }), configured)
  })
})

await test('an unusable configured path is reported, never skipped for another candidate', async () => {
  await withTempDir('ops-platform-missing', async dir => {
    const missing = path.join(dir, 'missing-worker')
    assert.throws(() => resolveWorker({ binaryPath: missing }, { platform: 'openharmony', arch: 'arm64' }), error => {
      assert.match(error.message, /unavailable/)
      assert.ok(error.message.includes(missing), 'the message names the configured path')
      return true
    })
    assert.throws(() => resolveWorker({ binaryPath: dir }, { platform: 'openharmony', arch: 'arm64' }), /unavailable/)
    assert.throws(() => resolveWorker({ binaryPath: 'relative-worker' }), /absolute path/)
  })
})

await test('a worker staged for another ABI family validates against its own directory', async () => {
  await withTempDir('ops-platform-alias', async dir => {
    const file = stage(dir, { target: 'linux-arm64' })
    assert.equal(resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), file)
    assert.equal(resolveWorker({}, { platform: 'openharmony', arch: 'arm64', env: { DSH_OPS_WORKER_BIN: file } }), file)
  })
})

await test('a receipt that disagrees with the directory it was staged into is rejected', async () => {
  await withTempDir('ops-platform-mislabeled', async dir => {
    const file = stage(dir, { target: 'linux-arm64', receiptTarget: 'linux-x64' })
    assert.throws(() => resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), /provenance mismatch/)
  })
})

await test('bytes changed after staging still fail the receipt hash', async () => {
  await withTempDir('ops-platform-tamper', async dir => {
    const file = stage(dir, { target: 'linux-arm64', tamper: true })
    assert.throws(() => resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), /provenance mismatch/)
  })
})

await test('a receipt version that disagrees with the package is rejected', async () => {
  await withTempDir('ops-platform-version', async dir => {
    const file = stage(dir, { target: 'linux-arm64', receiptVersion: '0.0.0' })
    assert.throws(() => resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), /provenance mismatch/)
  })
})

await test('automatic resolution still requires the staged target to be this host', async () => {
  await withTempDir('ops-platform-strict', async dir => {
    stage(dir, { target: 'win32-x64' })
    assert.ok(resolveWorker({}, { packageRoot: dir, platform: 'win32', arch: 'x64' }).startsWith(dir))
    assert.throws(() => resolveWorker({}, { packageRoot: dir, platform: 'win32', arch: 'arm64' }), /Windows x64 only/)
  })
})

await test('an unreadable receipt is a provenance failure, not a raw filesystem error', async () => {
  await withTempDir('ops-platform-receipt', async dir => {
    const file = stage(dir, { target: 'linux-arm64' })
    fs.rmSync(path.join(path.dirname(file), 'provenance.json'))
    assert.throws(() => resolveWorker({ binaryPath: file }, { platform: 'openharmony', arch: 'arm64' }), /provenance is unavailable or unreadable/)
  })
})

report('platform')
