import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useNotificationStore } from '@/core/stores/notification';
import { findIosArchive, iosArchivePage } from '@/core/stores/iosArchiveUpdate';

const repository = 'yangxionglin/VCPMobile-durable';
const build = { branch: 'codex/vcpmobile-ios', sha: 'a'.repeat(40), number: 14, repositoryId: 123 };
const latest = {
  id: 6500, run_number: 15, run_attempt: 2, head_sha: 'b'.repeat(40), head_branch: build.branch,
  head_repository: { full_name: repository, id: 123 }, path: '.github/workflows/ios-archive.yml',
  conclusion: 'success', event: 'workflow_dispatch', display_title: 'iOS release archive · ' + build.branch,
};
const artifact = {
  name: 'ios-release-' + latest.head_sha + '-2', expired: false, size_in_bytes: 20480, expires_at: '2099-01-01T00:00:00Z',
};

function mockRuns(run = latest, file = artifact) {
  const fetcher = vi.fn().mockResolvedValueOnce(Response.json({ workflow_runs: [run] }))
    .mockResolvedValueOnce(Response.json({ artifacts: [file] }));
  vi.stubGlobal('fetch', fetcher);
  return fetcher;
}

beforeEach(() => { localStorage.clear(); setActivePinia(createPinia()); });
afterEach(() => { vi.unstubAllGlobals(); vi.unstubAllEnvs(); });

describe('iOS custom archive notifications', () => {
  it.each(['push', 'workflow_dispatch'])('finds a successful same-channel %s artifact without credentials', async event => {
    const fetcher = mockRuns({ ...latest, event });
    const available = await findIosArchive(build);
    expect(available).toMatchObject({ number: 15, id: 6500, sha: latest.head_sha });
    const url = new URL(fetcher.mock.calls[0][0]);
    expect(url.origin + url.pathname).toBe('https://api.github.com/repos/' + repository + '/actions/workflows/ios-archive.yml/runs');
    expect(url.searchParams.get('branch')).toBe(build.branch);
    expect(url.searchParams.has('event')).toBe(false);
    expect(fetcher.mock.calls[0][1]).toMatchObject({ credentials: 'omit', referrerPolicy: 'no-referrer', redirect: 'error' });
    expect(iosArchivePage(available!.id)).toBe('https://github.com/' + repository + '/actions/runs/6500');
  });

  it.each([
    { run_number: build.number }, { head_sha: build.sha }, { head_branch: 'different' },
    { head_repository: { full_name: repository, id: 124 } }, { path: 'fork/workflow.yml' },
    { conclusion: 'failure' }, { event: 'pull_request' }, { display_title: 'iOS debug archive · ' + build.branch },
  ])('ignores another channel, commit replay or rejected build %o', async fields => {
    const fetcher = mockRuns({ ...latest, ...fields });
    expect(await findIosArchive(build)).toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it.each([{ expired: true }, { expires_at: '2000-01-01' }, { name: 'wrong-attempt' }, { size_in_bytes: 0 }])(
    'requires a downloadable archive %o', async fields => {
      mockRuns(latest, { ...artifact, ...fields });
      expect(await findIosArchive(build)).toBeNull();
    },
  );

  it('backs off offline, allows manual retry, and emits one notice per available build', async () => {
    vi.stubEnv('VITE_VCP_IOS_ARCHIVE', JSON.stringify(build));
    vi.resetModules();
    const { useIosArchiveUpdate } = await import('@/core/stores/iosArchiveUpdate');
    const store = useIosArchiveUpdate();
    const offline = vi.fn().mockRejectedValue(new TypeError('network'));
    vi.stubGlobal('fetch', offline);
    await store.check(false);
    await store.check(false);
    expect(store.failed).toBe(true);
    expect(offline).toHaveBeenCalledOnce();
    mockRuns();
    await store.check(true);
    expect(store.available?.id).toBe(latest.id);
    const before = vi.fn();
    vi.stubGlobal('fetch', before);
    await store.check(false);
    expect(before).not.toHaveBeenCalled();
    const checkKey = 'vcp_ios_update:' + build.branch + ':' + build.sha;
    // A new launch a day later can discover the same installed and available builds again.
    for (let i = 0; i < 2; i++) {
      setActivePinia(createPinia());
      localStorage.removeItem(checkKey);
      mockRuns({ ...latest, id: latest.id + i, run_number: latest.run_number + i, event: i === 0 ? 'push' : 'workflow_dispatch' });
      await useIosArchiveUpdate().check(false);
      expect(useNotificationStore().activeToasts).toHaveLength(i === 0 ? 1 : 0);
    }
  });
});
