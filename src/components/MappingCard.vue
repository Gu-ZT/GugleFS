<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";
import type { MappingRuntime } from "../types";
import { endpointOf, hasPersistedAuthentication } from "../types";

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
      return t("mapping.unmount");
    case "mounting":
      return t("mapping.mounting");
    case "unmounting":
      return t("mapping.unmounting");
    default:
      return t("mapping.mount");
  }
});
</script>

<template>
  <article
    class="link-card"
    :data-state="runtime.state"
    :aria-label="t('mapping.aria', { name: config.name })"
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
        {{ t(`status.${runtime.state}`) }}
      </span>
    </div>

    <div class="link-route">
      <span
        class="route-node route-local"
        :aria-label="t('mapping.localMount', { mountPoint: config.mountPoint })"
      >
        {{ config.mountPoint }}
      </span>
      <span class="route-line" aria-hidden="true"><span class="route-signal"></span></span>
      <span
        class="route-node route-remote"
        :title="endpoint"
        :aria-label="t('mapping.remoteAddress', { endpoint })"
      >
        {{ endpoint }}
      </span>
    </div>

    <p v-if="runtime.lastError" class="card-error" role="alert">
      {{ runtime.lastError }}
    </p>

    <div class="card-foot">
      <div class="card-meta">
        <span class="meta-row">
          <span class="meta-label">{{ t("mapping.credentials") }}</span>
          <strong :class="{ 'credential-stored': credentialStored }">
            {{ t(credentialStored ? "mapping.saved" : "mapping.notSaved") }}
          </strong>
        </span>
        <span v-if="config.autoMount" class="meta-row">
          <span class="meta-label">{{ t("mapping.autoMount") }}</span>
        </span>
        <span v-if="config.sftpTotpRequired" class="meta-row">
          <span class="meta-label">{{ t("mapping.mfaRequired") }}</span>
        </span>
      </div>
      <div class="card-actions">
        <button
          type="button"
          :class="[runtime.state === 'mounted' ? 'danger' : 'primary', 'compact']"
          :disabled="busy"
          :aria-label="t('mapping.actionAria', { action: mountLabel, name: config.name })"
          :aria-busy="busy"
          @click="emit('toggleMount', runtime)"
        >
          {{ mountLabel }}
        </button>
        <button
          type="button"
          class="ghost compact"
          :disabled="locked"
          :aria-label="t('mapping.editAria', { name: config.name })"
          @click="emit('edit', runtime)"
        >
          {{ t("mapping.edit") }}
        </button>
        <button
          type="button"
          class="ghost danger-text compact"
          :disabled="locked"
          :aria-label="t('mapping.deleteAria', { name: config.name })"
          @click="emit('remove', config.id)"
        >
          {{ t("mapping.delete") }}
        </button>
      </div>
    </div>
  </article>
</template>
