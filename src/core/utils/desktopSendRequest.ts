import { parseLocalPreferenceObject, readLocalPreference, writeLocalPreference } from '../preferences/localPreference';

const byTopic = new Map<string, Map<string, string>>();
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function topicKey(ownerType: string, ownerId: string, topicId: string): string {
  return 'vcp.desktop.pendingSend.' + JSON.stringify([ownerType, ownerId, topicId]);
}

function pendingMessages(key: string): Map<string, string> {
  const cached = byTopic.get(key);
  if (cached) return cached;
  const pending = new Map<string, string>();
  const stored = readLocalPreference(key);
  if (stored.ok && stored.raw) {
    for (const [hash, id] of Object.entries(parseLocalPreferenceObject(stored.raw))) {
      if (/^[0-9a-f]{64}$/.test(hash) && typeof id === 'string' && uuid.test(id)) pending.set(hash, id);
    }
  }
  byTopic.set(key, pending);
  return pending;
}

export async function desktopSendRequestId(ownerType: string, ownerId: string, topicId: string, text: string): Promise<string> {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  const hash = Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2, '0')).join('');
  const key = topicKey(ownerType, ownerId, topicId);
  const pending = pendingMessages(key);
  const previous = pending.get(hash);
  if (previous) return previous;

  const id = crypto.randomUUID();
  pending.set(hash, id);
  writeLocalPreference(key, JSON.stringify(Object.fromEntries(pending)));
  return id;
}

export function forgetDesktopSendRequest(ownerType: string, ownerId: string, topicId: string, id: string): void {
  const key = topicKey(ownerType, ownerId, topicId);
  const pending = pendingMessages(key);
  for (const [hash, requestId] of pending) {
    if (requestId !== id) continue;
    pending.delete(hash);
    if (pending.size) {
      writeLocalPreference(key, JSON.stringify(Object.fromEntries(pending)));
    } else {
      try {
        localStorage.removeItem(key);
        byTopic.delete(key);
      } catch {
        // Retain the empty map so a temporarily unavailable preference cannot revive an acknowledged ID.
      }
    }
    return;
  }
}
