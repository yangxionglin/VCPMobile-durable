<script setup lang="ts">
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { useRouter } from "vue-router";
import { useTopicStore } from "../../core/stores/topicListManager";
import { useChatSessionStore } from "../../core/stores/chatSessionStore";
import { useAssistantStore } from "../../core/stores/assistant";
import { useLayoutStore } from "../../core/stores/layout";
import { useNotificationStore } from "../../core/stores/notification";
import { useOverlayStore } from "../../core/stores/overlay";

const topicStore = useTopicStore();
const sessionStore = useChatSessionStore();
const assistantStore = useAssistantStore();
const layoutStore = useLayoutStore();
const notificationStore = useNotificationStore();
const overlayStore = useOverlayStore();
const router = useRouter();

const isCreating = ref(false);
const isContinuing = ref(false);
const continuationSource = computed(() => {
  const key = sessionStore.currentConversationKey;
  return key?.ownerType === 'group' && key.topicId.startsWith('group_topic_') ? key : null;
});
let pendingHandoff: {
  groupId: string;
  topicId: string;
  digest: string;
  text: string;
  requestId: string;
} | null = null;

async function handleContinuation() {
  const source = continuationSource.value;
  if (!source || isContinuing.value) return;
  isContinuing.value = true;
  try {
    const prior = pendingHandoff?.groupId === source.ownerId && pendingHandoff.topicId === source.topicId
      ? pendingHandoff : null;
    const preview = prior ?? {
      ...await invoke<{ digest: string; text: string }>("preview_continuation_topic", {
        groupId: source.ownerId, topicId: source.topicId,
      }),
      groupId: source.ownerId,
      topicId: source.topicId,
      requestId: crypto.randomUUID(),
    };
    if (continuationSource.value?.epoch !== source.epoch) return;
    const confirmed = await overlayStore.showConfirm({
      title: "从当前话题新建接续话题",
      message: preview.text,
    });
    if (!confirmed || continuationSource.value?.epoch !== source.epoch) return;
    pendingHandoff = preview;
    const result = await invoke<{ topicName: string }>("create_continuation_topic", {
      groupId: preview.groupId, topicId: preview.topicId, digest: preview.digest, requestId: preview.requestId,
    });
    pendingHandoff = null;
    notificationStore.addNotification({
      type: 'success', title: '接续话题已创建',
      message: `${result.topicName}，同步完成后可在话题列表打开。`, toastOnly: true,
    });
  } catch (error) {
    const message = String(error);
    if (message.includes('项目状态已变化')) pendingHandoff = null;
    notificationStore.addNotification({ type: 'error', title: '创建接续话题未完成', message, toastOnly: true });
  } finally {
    isContinuing.value = false;
  }
}

const currentItemId = computed(
  () =>
    sessionStore.currentSelectedItem?.id || assistantStore.agents[0]?.id || null,
);
const canCreateTopic = computed(
  () => Boolean(currentItemId.value) && !isCreating.value,
);

const handleCreateTopic = async () => {
  if (isCreating.value) return;

  console.info(
    "[TopicCreator] create-topic clicked",
    sessionStore.currentSelectedItem,
  );

  const owner = sessionStore.currentSelectedItem;
  const ownerId = owner?.id || currentItemId.value;
  if (!ownerId) {
    notificationStore.addNotification({
      type: 'warning',
      message: '请先选择一个助手或群组',
      toastOnly: true
    });
    return;
  }

  isCreating.value = true;
  const ownerType = owner?.type || (
    assistantStore.agents.some((a) => a.id === ownerId) ? "agent" : "group"
  );
  const intent = sessionStore.beginNavigation();

  const newTopicName = `新话题 ${new Date().toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  })}`;

  try {
    const newTopic = await topicStore.createTopic(
      ownerId,
      ownerType,
      newTopicName,
    );
    if (!newTopic?.id || !sessionStore.isNavigationCurrent(intent)) return;
    try {
      const result = await sessionStore.selectTopicById(ownerId, ownerType, newTopic.id, intent);
      if (!sessionStore.isNavigationCurrent(intent)) return;
      if (result === 'accepted') {
        layoutStore.setLeftDrawer(false);
        if (router.currentRoute.value.path !== "/chat") await router.push("/chat");
      } else sessionStore.notifyNavigationResult(result);
    } catch (error) {
      if (sessionStore.isNavigationCurrent(intent)) {
        notificationStore.addNotification({ type: 'error', title: '话题已创建，打开失败', message: String(error), toastOnly: true });
      }
    }
  } catch (error) {
    console.error("[TopicCreator] create-topic failed", error);
    // 错误通知已在 store 层处理
  } finally {
    isCreating.value = false;
  }
};
</script>

<template>
  <button
    class="w-full py-2.5 bg-green-500/10 dark:bg-green-500/20 hover:bg-green-500/20 dark:hover:bg-green-500/30 text-green-600 dark:text-green-400 rounded-xl text-sm font-bold transition-all flex items-center justify-center gap-2 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-green-500/10 disabled:dark:hover:bg-green-500/20"
    :disabled="!canCreateTopic" @click="handleCreateTopic">
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <line x1="12" y1="5" x2="12" y2="19"></line>
      <line x1="5" y1="12" x2="19" y2="12"></line>
    </svg>
    新建话题
  </button>
  <button
    v-if="continuationSource"
    type="button"
    class="mt-2 min-h-11 w-full rounded-xl border border-black/10 text-sm font-medium dark:border-white/10 disabled:opacity-40"
    :disabled="isContinuing"
    @click="handleContinuation"
  >
    {{ isContinuing ? '正在处理接续话题…' : '从当前话题新建接续话题' }}
  </button>
</template>
