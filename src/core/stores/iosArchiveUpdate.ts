import { defineStore } from 'pinia';
import { ref } from 'vue';
import { readLocalPreference, writeLocalPreference } from '../preferences/localPreference';
import { useNotificationStore } from './notification';

const repository = 'yangxionglin/VCPMobile-durable';
const api = 'https://api.github.com/repos/' + repository + '/actions';
const shaPattern = /^[a-f0-9]{40}$/;
const positiveId = (value: unknown): value is number => typeof value === 'number' && Number.isSafeInteger(value) && value > 0;

interface Build { branch: string; sha: string; number: number; repositoryId: number }
interface Run {
  id: number; run_number: number; run_attempt: number; head_branch: string; head_sha: string;
  path: string; conclusion: string; event: string; display_title: string;
  head_repository?: { full_name: string; id: number };
}
interface Artifact { name: string; expired: boolean; expires_at: string; size_in_bytes: number }
interface Available { number: number; id: number; sha: string; expires: number }

function installedBuild(): Build | null {
  try {
    const build: Build = JSON.parse(import.meta.env.VITE_VCP_IOS_ARCHIVE || 'null');
    return build && build.branch === 'codex/vcpmobile-ios'
      && shaPattern.test(build.sha) && positiveId(build.number) && positiveId(build.repositoryId) ? build : null;
  } catch { return null; }
}

export const iosArchiveBuild = installedBuild();

async function readApi(path: string, signal: AbortSignal) {
  const response = await fetch(api + path, {
    signal, redirect: 'error', credentials: 'omit', referrerPolicy: 'no-referrer',
  });
  if (!response.ok) throw new Error('GitHub HTTP ' + response.status);
  return response.json();
}

export async function findIosArchive(build: Build): Promise<Available | null> {
  const signal = AbortSignal.timeout(12_000);
  const query = new URLSearchParams({ branch: build.branch, status: 'success', per_page: '100' });
  for (let page = 1; page <= 10; page++) {
    query.set('page', String(page));
    const data = await readApi('/workflows/ios-archive.yml/runs?' + query, signal);
    const runs: Run[] = data.workflow_runs;
    if (!Array.isArray(runs)) throw Error('Invalid workflow response');
    const latest = runs.find(run => run.path === '.github/workflows/ios-archive.yml'
      && (run.event === 'push' || run.event === 'workflow_dispatch') && run.conclusion === 'success'
      && run.head_repository?.id === build.repositoryId && run.head_repository.full_name === repository
      && run.head_branch === build.branch && run.display_title === 'iOS release archive · ' + build.branch
      && shaPattern.test(run.head_sha) && positiveId(run.id) && positiveId(run.run_number) && positiveId(run.run_attempt));
    if (latest) {
      if (latest.run_number <= build.number || latest.head_sha === build.sha) return null;
      const result = await readApi('/runs/' + latest.id + '/artifacts', signal);
      const artifacts: Artifact[] = result.artifacts;
      if (!Array.isArray(artifacts)) throw Error('Invalid artifacts response');
      const artifact = artifacts.find(item => item.name === 'ios-release-' + latest.head_sha + '-' + latest.run_attempt
        && item.expired === false && item.size_in_bytes > 0 && Date.parse(item.expires_at) > Date.now());
      if (!artifact) return null;
      return { id: latest.id, number: latest.run_number, sha: latest.head_sha, expires: Date.parse(artifact.expires_at) };
    }
    if (runs.length < 100) return null;
  }
  throw Error('Workflow listing incomplete');
}

export function iosArchivePage(runId: number) {
  if (!positiveId(runId)) throw Error('无效构建编号');
  return 'https://github.com/' + repository + '/actions/runs/' + runId;
}

export const useIosArchiveUpdate = defineStore('iosArchiveUpdate', () => {
  const available = ref<Available | null>(null);
  const checking = ref(false);
  const checked = ref(false);
  const failed = ref(false);
  const notifications = useNotificationStore();
  let pending: Promise<void> | undefined;
  let memoryRetry = 0;

  async function check(manual: boolean) {
    const build = iosArchiveBuild;
    if (!build || pending) return pending;
    const key = 'vcp_ios_update:' + build.branch + ':' + build.sha;
    const saved = readLocalPreference(key);
    const next = Math.max(memoryRetry, saved.ok ? Number(saved.raw) || 0 : 0);
    const now = Date.now();
    if (available.value && available.value.expires <= now) available.value = null;
    if (!manual && next > now && next <= now + 86400_000) return;
    checking.value = true;
    failed.value = false;
    pending = (async () => {
      try {
        available.value = await findIosArchive(build);
        checked.value = true;
        memoryRetry = Date.now() + 86400_000;
        const latest = available.value;
        if (latest && !manual) {
          const notified = readLocalPreference(key + ':noticed');
          if (!notified.ok || notified.raw !== latest.sha) {
            notifications.addNotification({ type: 'info', id: 'ios-archive-' + latest.sha, oncePerWebview: true,
              title: 'VCPMobile 定制版可更新', message: '构建 #' + latest.number + ' 已就绪，可在设置的「版本更新」查看。' });
            writeLocalPreference(key + ':noticed', latest.sha);
          }
        }
      } catch {
        failed.value = true;
        memoryRetry = Date.now() + 15 * 60_000;
      } finally {
        writeLocalPreference(key, String(memoryRetry));
        checking.value = false;
        pending = undefined;
      }
    })();
    return pending;
  }
  return { available, checking, checked, failed, check };
});
