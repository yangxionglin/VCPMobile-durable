import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const loadRequests = () => import('@/core/utils/desktopSendRequest');
const topic = ['group', 'crew', 'topic'] as const;

beforeEach(() => {
  localStorage.clear();
  vi.resetModules();
});

afterEach(() => vi.unstubAllGlobals());

it('persists only hashes and UUIDs and recognizes concurrent retries and reopened conversations', async () => {
  const text = '继续处理这份工单';
  const { desktopSendRequestId } = await loadRequests();
  const [a, b] = await Promise.all([desktopSendRequestId(...topic, text), desktopSendRequestId(...topic, text)]);
  expect(a).toEqual(b);
  const encoded = JSON.parse(localStorage.getItem(localStorage.key(0)!)!);
  expect(Object.keys(encoded)).toHaveLength(1);
  expect(Object.keys(encoded)[0]).toMatch(/^[a-f0-9]{64}$/);
  expect(Object.values(encoded)).toEqual([a]);
  const other = await desktopSendRequestId('agent', 'crew', 'topic', text);
  expect(other).not.toEqual(a);
  expect(await desktopSendRequestId('group', 'second', 'topic', text)).not.toEqual(a);
  expect(await desktopSendRequestId('group', 'crew', 'other', text)).not.toEqual(a);
  vi.resetModules();
  expect(await (await loadRequests()).desktopSendRequestId(...topic, text)).toEqual(a);
});

it('acknowledges only the matching request, including when delayed receipts arrive out of order', async () => {
  const { desktopSendRequestId, forgetDesktopSendRequest } = await loadRequests();
  const first = await desktopSendRequestId(...topic, 'first');
  const second = await desktopSendRequestId(...topic, 'second');
  expect(second).not.toEqual(first);
  forgetDesktopSendRequest(...topic, first);
  const next = await desktopSendRequestId(...topic, 'first');
  expect(next).not.toEqual(first);
  forgetDesktopSendRequest(...topic, first);
  expect(await desktopSendRequestId(...topic, 'first')).toEqual(next);
  expect(await desktopSendRequestId(...topic, 'second')).toEqual(second);
  forgetDesktopSendRequest(...topic, next);
  forgetDesktopSendRequest(...topic, second);
  expect(localStorage.length).toBe(0);
});

it('retains requests for the current session if local storage is unavailable', async () => {
  vi.stubGlobal('localStorage', undefined);
  const { desktopSendRequestId, forgetDesktopSendRequest } = await loadRequests();
  const first = await desktopSendRequestId(...topic, 'text');
  expect(await desktopSendRequestId(...topic, 'text')).toBe(first);
  forgetDesktopSendRequest(...topic, first);
  expect(await desktopSendRequestId(...topic, 'text')).not.toBe(first);
});
