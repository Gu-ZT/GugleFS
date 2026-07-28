<script setup lang="ts">
import { computed } from "vue";
import type { MappingRuntime } from "../types";
import { endpointOf, hasPersistedAuthentication, statusLabel } from "../types";

const props = defineProps<{ runtime: MappingRuntime }>();

const emit = defineEmits<{
  toggleMount: [runtime: MappingRuntime];
  edit: [runtime: MappingRuntime];
  remove: [id: string];
}>();

const config = computed(() => props.runtime.config);
const endpoint = computed(() => endpointOf(config.value));
const credentialStored = computed(() => hasPersistedAuthentication(config.value));
const busy = computed(
  () => props.runtime.state === "mounting" || props.runtime.state === "unmounting",
);
const locked = computed(() => props.runtime.state !== "unmounted" && props.runtime.state !== "error");

const mountLabel = computed(() => {
  switch (props.runtime.state) {
    case "mounted":
      return "卸载";
    case "mounting":
      return "挂载中…";
    case "unmounting":
      return "卸载中…";
    default:
      return "挂载";
  }
});
</script>

<template>
  <article
    class="link-card"
    :data-state="runtime.state"
    :aria-label="`${config.name} 映射`"
  >
    <div class="card-head">
      <span class="protocol-badge" :data-protocol="config.protocol">
        {{ config.protocol.toUpperCase() }}
      </span>
      <h3 class="card-name">{{ config.name }}</h3>
      <span
        class="status-pill"
        :class="`status-${runtime.state}`"
        :title="runtime.lastError ?? undefined"
        role="status"
        aria-live="polite"
      >
        {{ statusLabel(runtime.state) }}
      </span>
    </div>

    <div class="link-route">
      <span class="route-node route-local" :aria-label="`本地挂载点 ${config.mountPoint}`">
        {{ config.mountPoint }}
      </span>
      <span class="route-line" aria-hidden="true"><span class="route-signal"></span></span>
      <span class="route-node route-remote" :title="endpoint" :aria-label="`远程地址 ${endpoint}`">
        {{ endpoint }}
      </span>
    </div>

    <p v-if="runtime.lastError" class="card-error" role="alert">
      {{ runtime.lastError }}
    </p>

    <div class="card-foot">
      <div class="card-meta">
        <span class="meta-row">
          <span class="meta-label">凭据</span>
          <strong :class="{ 'credential-stored': credentialStored }">
            {{ credentialStored ? "已保存" : "未保存" }}
          </strong>
        </span>
        <span v-if="config.autoMount" class="meta-row">
          <span class="meta-label">解锁后自动挂载</span>
        </span>
        <span v-if="config.sftpTotpRequired" class="meta-row">
          <span class="meta-label">需要 MFA</span>
        </span>
      </div>
      <div class="card-actions">
        <button
          type="button"
          :class="[runtime.state === 'mounted' ? 'danger' : 'primary', 'compact']"
          :disabled="busy"
          :aria-label="`${mountLabel} ${config.name}`"
          :aria-busy="busy"
          @click="emit('toggleMount', runtime)"
        >
          {{ mountLabel }}
        </button>
        <button
          type="button"
          class="ghost compact"
          :disabled="locked"
          :aria-label="`编辑 ${config.name}`"
          @click="emit('edit', runtime)"
        >
          编辑
        </button>
        <button
          type="button"
          class="ghost danger-text compact"
          :disabled="locked"
          :aria-label="`删除 ${config.name}`"
          @click="emit('remove', config.id)"
        >
          删除
        </button>
      </div>
    </div>
  </article>
</template>
