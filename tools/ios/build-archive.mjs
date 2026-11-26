import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const profile = process.argv[2];
if (!['release', 'debug'].includes(profile)) throw new Error('Expected archive profile: release or debug');
if (process.platform !== 'darwin' || process.env.GITHUB_ACTIONS !== 'true') {
  throw new Error('Run the iOS Device Archive workflow on the macOS runner.');
}

const root = new URL('../../', import.meta.url);
const config = JSON.parse(readFileSync(new URL('src-tauri/tauri.ios.conf.json', root), 'utf8'));
const xcrun = (...args) => execFileSync('xcrun', args, { encoding: 'utf8' }).trim();
const env = {
  ...process.env,
  IPHONEOS_DEPLOYMENT_TARGET: config.bundle.iOS.minimumSystemVersion,
  SDKROOT: xcrun('--sdk', 'iphoneos', '--show-sdk-path'),
  CC_aarch64_apple_ios: xcrun('--sdk', 'iphoneos', '--find', 'clang'),
  CXX_aarch64_apple_ios: xcrun('--sdk', 'iphoneos', '--find', 'clang++'),
  AR_aarch64_apple_ios: xcrun('--find', 'ar'),
};
const releaseBranch = 'codex/vcpmobile-ios';
delete env.VITE_VCP_IOS_ARCHIVE;
if (profile === 'release' && process.env.GITHUB_REPOSITORY === 'yangxionglin/VCPMobile-durable'
  && process.env.GITHUB_REF === 'refs/heads/' + releaseBranch
  && ['push', 'workflow_dispatch'].includes(process.env.GITHUB_EVENT_NAME)
  && process.env.GITHUB_WORKFLOW_REF === 'yangxionglin/VCPMobile-durable/.github/workflows/ios-archive.yml@refs/heads/' + releaseBranch) {
  env.VITE_VCP_IOS_ARCHIVE = JSON.stringify({
    branch: releaseBranch, sha: process.env.GITHUB_SHA,
    number: Number(process.env.GITHUB_RUN_NUMBER), repositoryId: Number(process.env.GITHUB_REPOSITORY_ID),
  });
}
for (const key of [
  'LIBSQLITE3_SYS_USE_PKG_CONFIG', 'ZSTD_SYS_USE_PKG_CONFIG', 'RING_PREGENERATE_ASM',
  'APPLE_DEVELOPMENT_TEAM', 'IOS_CERTIFICATE', 'IOS_CERTIFICATE_PASSWORD', 'IOS_MOBILE_PROVISION',
  'APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_PROVISIONING_PROFILE',
  'APPLE_API_KEY', 'APPLE_API_KEY_PATH', 'APPLE_API_ISSUER',
]) delete env[key];
if (profile === 'debug') {
  env.CARGO_PROFILE_DEV_DEBUG = '1';
  env.CARGO_PROFILE_DEV_STRIP = 'false';
}
execFileSync('pnpm', [
  'tauri', 'ios', 'build', ...(profile === 'debug' ? ['--debug'] : []),
  '--target', 'aarch64', '--archive-only', '--no-sign', '--ci', '--', '--locked',
], { cwd: fileURLToPath(root), env, stdio: 'inherit' });
