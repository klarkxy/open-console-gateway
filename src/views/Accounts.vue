<template>
  <div class="accounts-view">
    <n-space vertical :size="16" class="accounts-content">
      <div class="accounts-toolbar">
        <n-space wrap class="accounts-actions">
          <n-button type="primary" @click="openAddModal">
            <template #icon>
              <n-icon :component="PlusOutlined" />
            </template>
            {{ t("新增账号") }}
          </n-button>
          <n-button @click="openTransfer('import')">{{ t("导入账号") }}</n-button>
          <n-button @click="openTransfer('export')">{{ t("导出账号") }}</n-button>
        </n-space>
        <div
          v-if="accountsLoaded && (accountPage.page?.totalCards ?? 0) > 0"
          class="accounts-filter-bar"
        >
          <div class="filter-field">
            <n-input v-model:value="searchFilter" :placeholder="t('搜索账号')" :aria-label="t('搜索账号')" :disabled="sortMode" size="small" clearable />
          </div>
          <div class="filter-field">
            <n-select
              v-model:value="planFilter"
              :options="planFilterOptions"
              :placeholder="t('按方案筛选')"
              :aria-label="t('按方案筛选')"
              :consistent-menu-width="false"
              :disabled="sortMode"
              size="small"
            />
          </div>
          <div class="filter-field">
            <n-select
              v-model:value="statusFilter"
              :options="statusFilterOptions"
              :placeholder="t('按状态筛选')"
              :aria-label="t('按状态筛选')"
              :consistent-menu-width="false"
              :disabled="sortMode"
              size="small"
            />
          </div>
          <n-button
            size="small"
            :type="sortMode ? 'primary' : 'default'"
            :disabled="sortMode ? orderSaving : !canEnterSortMode"
            @click="toggleSortMode"
          >
            {{ sortMode ? t("完成") : t("调整顺序") }}
          </n-button>
          <span v-if="sortMode" class="sort-mode-hint">{{ t("拖拽手柄调整顺序，点击“完成”退出") }}</span>
        </div>
      </div>

      <AccountRoutingControls :presentation="accountPage.page ? { routingMode: accountPage.page.routingMode, conversationSticky: accountPage.page.conversationSticky, revision: accountPage.page.revision } : null" @changed="revalidatePageAfterWrite" />

      <span id="account-order-instructions" class="sr-only">
        {{ t("使用上下方向键调整优先级") }}
      </span>

      <div
        v-if="accountListLoading"
        class="account-list-state"
        role="status"
        aria-live="polite"
        :aria-label="t('加载中…')"
      >
        <n-spin size="small" />
      </div>

      <n-alert v-else-if="accountListError && !accountsLoaded" type="error" :title="t('加载账号失败：{error}', { error: accountListError })">
        <n-button size="small" secondary @click="loadAccounts">{{ t("重试") }}</n-button>
      </n-alert>

      <n-alert
        v-if="catalogError"
        type="warning"
        :title="t('加载供应商目录失败：{error}', { error: catalogError })"
      >
        <n-button size="small" secondary :loading="catalogLoading" @click="loadProviderCatalog">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert
        v-if="identitiesError"
        type="warning"
        :title="t('加载身份投影失败：{error}', { error: identitiesError })"
      >
        <n-button
          size="small"
          secondary
          :loading="identitiesLoading"
          @click="loadIdentitiesOverlay"
        >{{ t("重试") }}</n-button>
      </n-alert>

      <n-alert
        v-if="destinationLoadFailed"
        type="error"
        :title="t(DESTINATION_LOAD_KEYS.load_failed, { error: destinationsStore.error })"
      >
        <n-button size="small" secondary :loading="destinationsStore.loading" @click="retryDestinations">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert
        v-if="destRefreshError"
        type="error"
        :title="t(DESTINATION_PROJECTION_REFRESH_KEYS[destRefreshError], { error: destRefreshErrorDetail })"
      >
        <n-button size="small" secondary :loading="destinationsStore.loading" @click="retryDestinations">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert
        v-if="destinationsStore.refusals.length > 0"
        type="error"
        :title="t('目的地投影失败：{count} 行无法映射', { count: destinationsStore.refusals.length })"
      >
        <div
          v-for="(row, index) in destinationsStore.refusals"
          :key="`${row.kind}:${row.id}:${index}`"
          class="mono"
        >
          {{ row.kind }} · {{ row.id }} · {{ row.detail }}
        </div>
        <n-button size="small" secondary :loading="destinationsStore.loading" @click="retryDestinations">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <PlatformAccountsSection
        v-if="legacyResourcesReady"
        ref="platformSectionRef"
        :accounts="accounts"
        @changed="loadAccounts"
        @destination-refresh-failed="handlePlatformDestinationRefreshFailure"
        @account-updated="replaceAccount"
      />

      <n-empty
        v-if="accountsLoaded && !sortMode && (accountPage.page?.cards.length ?? 0) === 0"
        :description="t('暂无账号')"
      >
        <template #extra>
          <n-button v-if="searchFilter || planFilter !== 'all' || statusFilter !== 'all'" @click="resetFilters">
            {{ t("重置") }}
          </n-button>
          <n-button v-else type="primary" @click="openAddModal">
            <template #icon>
              <n-icon :component="PlusOutlined" />
            </template>
            {{ t("新增账号") }}
          </n-button>
        </template>
      </n-empty>

      <div v-if="accountsLoaded && !sortMode" class="account-list">
        <n-alert v-if="accountPage.page?.errors.length" type="warning"
          :title="t('加载账号失败：{error}', { error: accountPage.page.errors.map(issue => `${issue.resource}${issue.id ? ` ${issue.id}` : ''}: ${issue.code}`).join(' · ') })">
          <n-button size="small" @click="loadAccounts">{{ t('重试') }}</n-button>
        </n-alert>
        <n-alert v-if="accountPage.error" type="warning" :title="t('加载账号失败：{error}', { error: accountPage.error })">
          <n-button size="small" @click="loadAccounts">{{ t('重试') }}</n-button>
        </n-alert>
        <AccountPageCard v-for="card in accountPage.page?.cards ?? []" :key="card.cardId" :card="card"
          :collapsed="collapsedCardIds.has(card.cardId)" :pending="busy || platformMutating"
          :offset="accountPage.cardPaging[card.cardId]?.offset ?? 0" :has-more="accountPage.cardPaging[card.cardId]?.hasMore ?? card.matchedCredentials > card.rows.length"
          :previous-offset="cardPageHistory[card.cardId]?.at(-1)"
          :loading="accountPage.cardLoading[card.cardId]" @toggle-collapse="toggleCardCollapse(card.cardId)"
          @arrange="toggleSortMode" @order-keydown="pageCardKeydown(card.cardId, $event)"
          @action="pageCardAction(card, $event)" @page="loadCardRows(card.cardId, $event)">
          <template #platform-models>
            <n-alert v-if="card.platform && platformPendingLink?.parentId === card.platform.id" type="warning" :show-icon="false">
              <div class="destination-pending-link">
                <span>{{ t('Key 已创建，关联尚未完成。') }}</span>
                <n-button size="tiny" secondary :loading="platformMutating" :disabled="platformMutating"
                  @click="platformSectionRef?.retryPendingLink()">{{ t('重试关联') }}</n-button>
              </div>
            </n-alert>
            <PlatformModelTable v-if="expandedPlatformCards.has(card.cardId) && platformSnapshotFor(card)"
              :snapshot="platformSnapshotFor(card)!" />
          </template>
          <n-alert v-if="accountPage.cardErrors[card.cardId]" type="warning"
            :title="t('加载账号失败：{error}', { error: accountPage.cardErrors[card.cardId] })">
            <n-button size="small" @click="loadCardRows(card.cardId, accountPage.cardPaging[card.cardId]?.offset ?? 0)">{{ t('重试') }}</n-button>
          </n-alert>
          <AccountPageCredentialRow v-for="row in card.rows" :key="row.credential.id" :row="row" :now="now"
            :cpa-status="accountPageCpaStatus(card)"
            :manual-receipt="manualReceiptFor(row)"
            :pending="!!pageActionPending[accountPageRowId(row)] || !!purchaseDateSaving[accountPageRowId(row)]"
            :refresh-state="accountPage.refreshStates[accountPageRowId(row)]"
            @visible="setPageRowVisible" @action="pageRowAction(row, $event)"
            @order-keydown="pageRowKeydown(row, $event)"
            @prepare-calibration="preparePageCalibration(row)"
            @update-purchase-date="pagePurchaseDate(row, $event)">
            <template #calibration>
              <CreditCalibrationEditor v-if="calibrationAccounts.has(accountPageRowId(row)) && row.billing?.credits"
                :account-id="accountPageRowId(row)" :binding="billingBinding(row.account?.updatedAt ?? '', row.inferenceEndpointUrl)"
                :status="row.billing" :now="now" @saved="revalidatePageAfterWrite" />
              <AccountUsageEditor v-else-if="calibrationAccounts.has(accountPageRowId(row)) && accountsStore.byId.get(accountPageRowId(row))"
                :account="accountsStore.byId.get(accountPageRowId(row))!" :usage="usageFor(accountPageRowId(row)).value"
                :limits="usageLimitsFor(accountsStore.byId.get(accountPageRowId(row))!)" :edits="usageEdits[accountPageRowId(row)] ?? {}" :now="now"
                @update-draft="(key, value) => updateUsageDraft(accountPageRowId(row), key, value)"
                @update-resets-first="(key, value) => updateResetsFirstField(accountPageRowId(row), key, value)"
                @update-resets-second="(key, value) => updateResetsSecondField(accountPageRowId(row), key, value)"
                @save="key => savePageUsage(row, key)" />
              <n-spin v-else size="small" />
            </template>
          </AccountPageCredentialRow>
        </AccountPageCard>
        <div v-if="accountPage.page && ((accountPage.page.offset ?? 0) > 0 || accountPage.page.hasMore)" class="account-page-pagination">
          <n-button :disabled="accountPage.loading || !accountPage.page.offset" @click="changePage(Math.max(0, accountPage.page.offset - accountPage.page.limit))">{{ t('上一页') }}</n-button>
          <span>{{ t('{count} 个 Key', { count: accountPage.page.matchedCredentials }) }}</span>
          <n-button :disabled="accountPage.loading || !accountPage.page.hasMore" @click="changePage(accountPage.page.offset + accountPage.page.limit)">{{ t('下一页') }}</n-button>
        </div>
      </div>

      <MotionConfig v-if="sortMode && accountsLoaded && displayedGroupViews.length > 0" reduced-motion="user">
      <div class="account-list">
        <component
          :is="cardWrapperComponent"
          v-for="view in displayedGroupViews"
          :key="view.group.id"
          v-bind="cardWrapperProps(view.group.id)"
          :data-layout-card-id="view.group.id"
        >
          <DestinationCard
            :group="view.displayGroup"
            :membership="view.group.credentials"
            :parent="view.parent"
            :accounts-by-id="accountsStore.byId"
            :catalog="providerCatalog"
            :links="cardLinksFor(view.parent)"
            :mutating="platformMutating || busy || groupDeletion?.id === view.group.destination.id"
            :importing="view.parent ? Boolean(platformStore.importing[view.parent.id]) : false"
            :refreshing="platformRefreshing"
            :pending-link="platformPendingLink"
            :now="now"
            :order-handle-disabled="!arrangementEnabled"
            :dragging="draggingCardId === view.group.id"
            :order-handle-hint="arrangementDisabledHint"
            :can-remove-empty-card="view.group.credentials.length === 0 && removableEmptyCardIds.has(view.group.id)"
            :can-delete-group="isDestinationDeletable(view.group.destination, destinationsStore.credentials)"
            :arranging-disabled="!arrangementEnabled"
            :cpa-status="cpaStatusFor(view.displayGroup)"
            :collapsed="!sortMode && collapsedCardIds.has(view.group.id)"
            :summary="cardSummaryFor(view.group)"
            :sort-mode="sortMode"
            :card-first="(cardPositions.get(view.group.id) ?? 0) === 0"
            :card-last="(cardPositions.get(view.group.id) ?? 0) === destinationsStore.cards.length - 1"
            @toggle-collapse="toggleCardCollapse(view.group.id)"
            @move-card="handleCardMove(view.group.id, $event)"
            @order-keydown="handleCardKeydown($event, view.group.id)"
            @order-drag-start="startCardDrag($event, view.group.id)"
            @add-card="addCardAfter(view.group.id)"
            @remove-empty-card="removeEmptyCardById(view.group.id)"
            @delete-group="confirmDeleteEmptyGroup(view.group.destination)"
            @refresh-parent="view.parent && queuePlatformParentRefresh(view.parent)"
            @edit="view.parent && platformSectionRef?.openEdit(view.parent)"
            @delete="view.parent && platformSectionRef?.confirmDelete(view.parent)"
            @add-key="addKeyForCard(view.group, view.parent)"
            @import-keys="view.parent && platformSectionRef?.importKeys(view.parent)"
            @link-existing="view.parent && platformSectionRef?.openLink(view.parent)"
            @retry-pending-link="platformSectionRef?.retryPendingLink()"
            @fetch-all-models="fetchAllPlatformModels(overlayAccountsFor(view.group))"
          >
            <template #row="{ credential, index, extraTags, figure, duplicateName, hideModelCount, modelCount }">
              <component
                :is="cardWrapperComponent"
                v-bind="rowWrapperProps(credential.id)"
                :data-layout-row-id="credential.id"
              >
                <CredentialRow
                v-bind="credentialRowBindingsFor(credential, view.displayGroup.destination)"
                :now="now"
                :extra-tags="extraTags"
                :figure="figure"
                :duplicate-name="duplicateName"
                :hide-model-count="hideModelCount"
                :model-count="modelCount"
                :menu-options="rowMenuOptionsFor(view.group, credential, index, view.parent)"
                :order-disabled="!arrangementEnabled || view.group.credentials.length < 2"
                :dragging="draggingCredentialId === credential.id"
                :quota-retrying="!!quotaRetrying[credential.id]"
                :cpa-status="cpaStatusFor(view.displayGroup)"
                :sort-mode="sortMode"
                :refresh-state="refreshStates[credential.legacy_account_id]"
                @order-drag-start="startCredentialDrag($event, view.group.id, credential.id)"
                @order-keydown="handleRowKeydown($event, credential.legacy_account_id)"
                :usage-read-blocked="!!view.parent && (platformMutating || busy)"
                @toggle="toggleAccount(credential.legacy_account_id)"
                @update-purchase-date="updatePurchaseDate(credential.legacy_account_id, $event)"
                @reload-usage="loadAccountUsage(credential.legacy_account_id)"
                @open-wizard="openManagedWizard(credential.legacy_account_id)"
                @menu-select="handleMenuSelect($event, credential.legacy_account_id, view.parent)"
                @usage-editor-open="focusUsageEditor(credential.legacy_account_id)"
                @usage-update-draft="(key, value) => updateUsageDraft(credential.legacy_account_id, key, value)"
                @usage-update-resets-first="(key, value) => updateResetsFirstField(credential.legacy_account_id, key, value)"
                @usage-update-resets-second="(key, value) => updateResetsSecondField(credential.legacy_account_id, key, value)"
                @usage-save="(key) => saveUsage(credential.legacy_account_id, key)"
                @retry-quota="retryQuotaRecovery(credential.id)"
                @open-models="openPlatformKeyModels(credential.legacy_account_id)"
              />
              </component>
            </template>
          </DestinationCard>
        </component>
      </div>
      </MotionConfig>

      <span class="sr-only" aria-live="polite" aria-atomic="true">{{ orderAnnouncement }}</span>
    </n-space>

    <AccountAddModal
      :show="showAddModal"
      @update:show="setAddModalVisible"
      :catalog="providerCatalog"
      :catalog-loading="catalogLoading"
      :connections="providersStore.connections"
      :managed-available="managedRegistrationAvailable"
      :managed-reason="managedRegistrationReason"
      :invite-missing="!opencodeInviteUrl"
      :create-busy="busy"
      :setup-pending="!!pendingNewCredits"
      :platform-busy="platformMutating"
      :initial-option-id="addInitialOptionId"
      @register-managed="openManagedCreateModal"
      @open-invite-url="openInviteUrl"
      @save-account="onFormSave"
      @create-platform="handleCreatePlatform"
      @preset-committed="onPresetAccountCommitted"
      @preset-conflict="onPresetAccountConflict"
    />

    <AccountFormModal
      ref="accountFormRef"
      :show="showModal"
      :account="editingAccount"
      :is-cooling="editingAccount ? isCooling(editingAccount, now) : false"
      :busy="busy"
      :catalog="providerCatalog"
      :endpoint-locked="!!editingPlatformLink"
      :endpoint-lock-hint="editingEndpointLockHint"
      @update:show="setAccountFormVisible"
      @save="onFormSave"
      @edit-connection="onEditConnection"
      @reset-cooldown="resetCooldown(editingAccount!.id)"
    />

    <AccountConnectionTestModal
      :show="!!testingAccount"
      :account="testingAccount"
      @update:show="setAccountTestVisible"
    />

    <n-modal
      :show="showManagedCreate"
      preset="card"
      :title="t('注册新账号')"
      class="account-managed-modal"
      style="width: 520px; max-width: calc(100vw - 32px)"
      :mask-closable="false"
      :close-on-esc="!busy"
      @update:show="setManagedCreateVisible"
    >
      <n-form label-placement="top" @submit.prevent="createManagedAccount">
        <n-form-item :label="t('名称')" required>
          <n-input
            v-model:value="managedDraft.name"
            autofocus
            :disabled="busy"
            :placeholder="t('例如：新账号 1')"
            :input-props="{ 'aria-label': t('名称') }"
          />
        </n-form-item>
        <n-form-item :label="t('邮箱备注（可选）')">
          <n-input
            v-model:value="managedDraft.username"
            :disabled="busy"
            :placeholder="t('仅作备注')"
            :input-props="{ 'aria-label': t('邮箱备注（可选）') }"
          />
        </n-form-item>
        <n-form-item
          :label="t('邀请链接')"
          required
          :show-feedback="true"
          :validation-status="managedInviteStatus"
          :feedback="managedInviteFeedback"
        >
          <n-input
            v-model:value="managedDraft.inviteUrl"
            :disabled="busy"
            class="mono"
            :placeholder="DEFAULT_OPENCODE_INVITE_URL"
            :input-props="{ 'aria-label': t('邀请链接') }"
            @blur="normalizeManagedInviteDraft"
          />
        </n-form-item>
      </n-form>
      <n-alert type="warning" :show-icon="false">
        {{ t("确认邀请链接是你自己的（默认仅演示）。修改会写入 OpenCode Go 供应商，草稿可随时继续。") }}
      </n-alert>
      <template #footer>
        <n-space justify="end">
          <n-button :disabled="busy" @click="setManagedCreateVisible(false)">
            {{ busy ? t("加载中…") : t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="busy"
            :disabled="!canCreateManagedDraft"
            @click="createManagedAccount"
          >{{ t("创建并开始") }}</n-button>
        </n-space>
      </template>
    </n-modal>

    <ManagedAccountWizard
      v-if="managedWizardAccount"
      :show="showManagedWizard"
      @update:show="setManagedWizardVisible"
      :account="managedWizardAccount"
      :browser-capabilities="browserCapabilities"
      :opening-target="openingBrowserTarget"
      :busy="busy"
      @open-browser="openAccountBrowser(managedWizardAccount.id, $event)"
      @advance="advanceManagedSetup(managedWizardAccount.id, $event)"
      @verify-key="verifyManagedKey(managedWizardAccount.id, $event)"
    />

    <AccountTransferModal
      v-model:show="showTransfer"
      :mode="transferMode"
      @imported="handleAccountsImported"
    />

    <AccountCredentialModal
      :show="showCredentialModal"
      :mode="credentialModalMode"
      :binding="credentialModalBinding"
      :connection="credentialModalConnection"
      :granted-endpoint-ids="credentialModalOperations?.grantedEndpointIds ?? []"
      :stale-endpoint-ids="credentialModalOperations?.staleEndpointIds ?? []"
      :stale-origins="credentialModalOperations?.staleOrigins ?? []"
      :unsupported-reason="credentialModalUnsupported"
      :busy="busy"
      @update:show="setCredentialModalVisible"
      @rotate="onRotateCredential"
      @save-binding="onPatchBinding"
    />

    <IdentityCredentialCreateModal
      ref="createModalRef"
      :show="showCreateModal"
      :unsupported-reason="createModalUnsupported"
      :busy="busy"
      :default-connection-id="createModalConnectionId"
      :connections="createModalConnections"
      :share-targets="createModalShareTargets"
      @update:show="setCreateModalVisible"
      @create="onCreateIdentityCredential"
    />

    <PlatformKeyModelsModal
      :show="platformKeyModelsAccount !== null"
      :account="platformKeyModelsAccount"
      :busy="platformMutating"
      @update:show="setPlatformKeyModelsVisible"
      @fetch="refreshPlatformKeyModels"
    />

    <n-modal
      :show="moveToCardState !== null"
      preset="card"
      :title="t('移动账号到卡片')"
      style="width: 440px; max-width: calc(100vw - 32px)"
      :close-on-esc="!orderSaving"
      @update:show="setMoveToCardVisible"
    >
      <n-space vertical :size="12">
        <p v-if="moveToCardState" class="move-to-card-subject">
          {{ moveToCardSubject }}
        </p>
        <n-radio-group
          v-if="moveToCardState"
          v-model:value="moveToCardTarget"
          class="move-to-card-options"
        >
          <n-radio
            v-for="option in moveToCardOptions"
            :key="option.value"
            :value="option.value"
            :disabled="orderSaving"
          >
            {{ option.label }}
          </n-radio>
        </n-radio-group>
      </n-space>
      <template #footer>
        <n-space justify="end">
          <n-button :disabled="orderSaving" @click="setMoveToCardVisible(false)">
            {{ t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="orderSaving"
            :disabled="!moveToCardTarget || !arrangementEnabled"
            @click="confirmMoveToCard"
          >{{ t("移动") }}</n-button>
        </n-space>
      </template>
    </n-modal>
  </div>
</template>

<script setup lang="ts">
import { accountPageQuotaReceipt } from "../domain/account-page.ts";
import { computed, defineAsyncComponent, nextTick, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NAlert,
  NButton,
  NEmpty,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NModal,
  NRadio,
  NRadioGroup,
  NSelect,
  NSpin,
  NSpace,
  useDialog,
  useMessage,
} from "naive-ui";
import { PlusOutlined } from "@vicons/antd";
import { DashboardRequestError, dashboardApi, isRevisionConflict } from "../api/dashboard";
import { providerApi } from "../api/providers.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useAccountPageStore, ACCOUNT_PAGE_SIZE } from "../stores/accountPage.ts";
import { invalidateManagementPages } from "../stores/managementPages.ts";
import { PAGE_READ_MAX_AGE_MS } from "../stores/readLifecycle.ts";
import type { AccountDetail, AccountPageCard as PageCard, AccountPageRow as PageRow } from "../api/pages.ts";
import { accountPageCpaStatus, accountPageDemandIds, accountPageRefreshErrorCodes, accountPageRowId } from "../domain/account-page.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useSessionStore } from "../stores/session.ts";
import { useIdentitiesStore } from "../stores/identities.ts";
import { useCpaStore } from "../stores/cpa.ts";
import { usePlatformAccountsStore } from "../stores/platformAccounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSettingsStore } from "../stores/settings.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type {
  Account,
  AccountInput,
  AccountSetupStep,
  AccountUpdate,
  BrowserCapabilities,
  BrowserTarget,
} from "../api/dashboard";
import { isCooling } from "../domain/accounts-usage.ts";
import {
  accountIsReady,
  accountMenuOptions,
  groupMoveMenuOptions,
  type AccountMenuOption,
} from "../domain/account-display.ts";
import {
  accountCredentialMenuOptions,
  credentialWriteSupport,
  isUncertainCreateFailure,
  type CredentialEditorMode,
} from "../domain/account-credential.ts";
import { identitiesApi } from "../api/identities.ts";
import type { BindingPatchInput, IdentityCredentialCreateInput } from "../api/identities.ts";
import { accountCapabilities, isManagedOnboardingAccount } from "../domain/account-capabilities.ts";
import { DEFAULT_PROVIDER_ID, connectionForDestination } from "../domain/destination-providers.ts";
import { legacyCustomAccountDestinationId } from "../domain/custom-account.ts";
import AccountRoutingControls from "../components/AccountRoutingControls.vue";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  includeCredentialRow,
  isSingleAccountGroup,
  isVacatedCustomShell,
  overlayAccountForCredential,
  type DestinationGroup,
} from "../domain/destination-groups.ts";
import {
  addEmptyCardAfter,
  buildRoutingCardGroups,
  moveCardInLayout,
  moveCredentialToCard,
  moveCredentialWithinCard,
  newRoutingCardId,
  removeEmptyCard,
  type RoutingCardMove,
} from "../domain/routing-cards.ts";
import {
  DESTINATION_LOAD_KEYS,
  DESTINATION_PROJECTION_REFRESH_KEYS,
  destinationFirstLoadFailed,
  refreshDestinationProjection as loadDestinationProjection,
  type DestinationProjectionRefreshCode,
} from "../domain/destination-projection-refresh.ts";
import { quotaRetryRequestNeeded, withAccountEnablement } from "../domain/quota-recovery.ts";
import type { CpaCardStatus } from "../domain/cpa-runtime.ts";
import { platformSnapshotErrorKey } from "../domain/platform-accounts.ts";
import { runAccountSaveFollowup } from "../domain/account-save-followup.ts";
import type { OnboardingIntent } from "../domain/onboarding-draft.ts";
import type { PlatformAccount, PlatformLink } from "../api/platform-accounts.ts";
import { useAccountUsage, type UsageLimitView } from "../domain/useAccountUsage.ts";
import { createAccountRefreshQueue, platformRefreshBinding, waitForAccountRefreshIdle, type AccountRefreshState } from "../domain/account-refresh-queue.ts";
import { accountInferenceEndpointUrl, officialBalanceSupported } from "../domain/upstream-balance.ts";
import { useRoutingCardLayout } from "./useRoutingCardLayout.ts";
import { MotionConfig, motion } from "motion-v";
import {
  accountStatusKey,
  filterAccounts,
  type AccountPlanFilter,
  type AccountStatusFilter,
} from "./account-filters.ts";
import {
  findPlanDefinition,
} from "../domain/plans.ts";
import { accountCreateRequestInput } from "../domain/account-create-payload.ts";
import {
  isFirstReadyProviderAccount,
  providerContractAllowsCatalogRefresh,
  shouldRefreshCatalogForNewProviderAccount,
} from "../domain/provider-catalog-refresh.ts";
import {
  mergeDiscoveredAccountCapabilities,
  mergeDiscoveredCatalogModels,
  usageCompanionCatalog,
  usageCompanionCatalogLockKey,
} from "../domain/usage-refresh-catalog.ts";
import {
  DESTINATION_EDIT_ISSUE_KEYS,
  destinationEditDraft,
  isDestinationEditable,
  isDestinationDeletable,
} from "../domain/destination-edit.ts";
import { planDestinationSave } from "../domain/destination-edit-save.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import {
  appViewRoute,
  readAccountAddDeepLink,
  readAccountAddReturn,
  readAccountDeepLink,
  routeQuerySearch,
  type ProviderScopeQuery,
} from "./app-navigation.ts";
import { mapWithConcurrency } from "../utils/async.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import {
  reconcileEditingAccount,
} from "./account-cas.ts";
import {
  DEFAULT_OPENCODE_INVITE_URL,
  browserViewUrl,
  normalizeOpenCodeInviteUrl,
} from "../domain/managed-account.ts";
// Modals load on demand instead of inflating the view chunk.
const AccountAddModal = defineAsyncComponent(() => import("../components/AccountAddModal.vue"));
const CredentialRow = defineAsyncComponent(() => import("../components/CredentialRow.vue"));
const DestinationCard = defineAsyncComponent(() => import("../components/DestinationCard.vue"));
import AccountPageCard from "../components/AccountPageCard.vue";
import AccountPageCredentialRow from "../components/AccountPageCredentialRow.vue";
const PlatformModelTable = defineAsyncComponent(() => import("../components/PlatformModelTable.vue"));
const AccountUsageEditor = defineAsyncComponent(() => import("../components/AccountUsageEditor.vue"));
const CreditCalibrationEditor = defineAsyncComponent(() => import("../components/CreditCalibrationEditor.vue"));
const AccountConnectionTestModal = defineAsyncComponent(() => import("../components/AccountConnectionTestModal.vue"));
const AccountFormModal = defineAsyncComponent(() => import("../components/AccountFormModal.vue"));
import type { AccountFormPayload } from "../components/AccountFormModal.vue";
const ManagedAccountWizard = defineAsyncComponent(() => import("../components/ManagedAccountWizard.vue"));
const AccountTransferModal = defineAsyncComponent(() => import("../components/AccountTransferModal.vue"));
const AccountCredentialModal = defineAsyncComponent(() => import("../components/AccountCredentialModal.vue"));
const IdentityCredentialCreateModal = defineAsyncComponent(() => import("../components/IdentityCredentialCreateModal.vue"));
import { useBillingStore } from "../stores/billing.ts";
import { billingBinding } from "../domain/billing.ts";
import type { CreditSetupInput } from "../domain/credit-setup.ts";
import PlatformAccountsSection from "../components/PlatformAccountsSection.vue";
const PlatformKeyModelsModal = defineAsyncComponent(() => import("../components/PlatformKeyModelsModal.vue"));
import type { PlatformAccountFormPayload } from "../components/PlatformAccountFormModal.vue";

const dialog = useDialog();
const message = useMessage();
const route = useRoute();
const router = useRouter();
const accountsStore = useAccountsStore();
const accountPage = useAccountPageStore();
const destinationsStore = useDestinationsStore();
const sessionStore = useSessionStore();
const identitiesStore = useIdentitiesStore();
const platformStore = usePlatformAccountsStore();
const providersStore = useProvidersStore();
const settingsStore = useSettingsStore();
const cpaStore = useCpaStore();
const refreshStates = ref<Record<string, AccountRefreshState>>({});
const refreshQueue = createAccountRefreshQueue((id, state) => {
  if (state) refreshStates.value[id] = state;
  else delete refreshStates.value[id];
});
// The account list lives in the store; the writable computed lets the
// usage composable and confirmed order receipts keep their Ref<Account[]> contract while every
// write commits through the store.
const accounts = computed<Account[]>({
  get: () => accountsStore.accounts,
  set: (list) => accountsStore.setAccounts(list),
});
const accountsLoaded = computed(() => accountPage.loaded);
const accountListError = computed(() => accountPage.error);
// The spinner gate covers only the first load; revalidations keep the
// current list rendered and commit silently when the response lands.
const accountListLoading = computed(() => {
  return !accountPage.loaded && !accountPage.error;
});
const destRefreshError = ref<DestinationProjectionRefreshCode | null>(null);
const destRefreshErrorDetail = ref("");
const destinationLoadFailed = computed(() => destinationFirstLoadFailed(
  destinationsStore.loaded,
  destinationsStore.error,
  destinationsStore.refusals.length,
));
const identitiesError = ref("");
const identitiesLoading = computed(() => identitiesStore.loading);
const testingAccountId = ref<string | null>(null);
const providerSettingsSaving = ref<Record<string, boolean>>({});
const purchaseDateSaving = ref<Record<string, boolean>>({});
const showModal = ref(false);
const showCredentialModal = ref(false);
const credentialModalMode = ref<CredentialEditorMode>("rotate");
const credentialModalAccountId = ref<string | null>(null);
const credentialModalExpectation = ref<MutationExpectation | null>(null);
const showCreateModal = ref(false);
const createModalCardId = ref<string | null>(null);
let accountViewSession = 0;
const createModalAccountId = ref<string | null>(null);
const createModalExpectation = ref<MutationExpectation | null>(null);
const createModalRef = ref<{ noteSaved(): void; noteFailure(error: unknown): void } | null>(null);
const accountFormRef = ref<{ noteSaved(): void } | null>(null);
const billingStore = useBillingStore();
watch(() => billingStore.sessionEpoch, () => { accountViewSession += 1; }, { flush: "sync" });
const groupDeletion = ref<{ id: string; operation: symbol } | null>(null);
watch(() => billingStore.sessionEpoch, () => { groupDeletion.value = null; }, { flush: "sync" });
let pendingCreditCreate: { created: Awaited<ReturnType<typeof identitiesApi.createIdentityCredential>>; credits: CreditSetupInput } | null = null;
let pendingCreditEdit: { account: Account; credits: CreditSetupInput } | null = null;
const pendingNewCredits = ref<{ account: Account; credits: CreditSetupInput } | null>(null);
watch(showCreateModal, show => { if (!show) pendingCreditCreate = null; });
watch(showModal, show => { if (!show) pendingCreditEdit = null; });
watch(() => billingStore.sessionEpoch, () => { pendingCreditCreate = null; pendingCreditEdit = null; pendingNewCredits.value = null; });

async function initializeAccountCredits(account: Account, credits: CreditSetupInput): Promise<void> {
  const binding = billingBinding(account.updated_at, accountInferenceEndpointUrl(account, identityForCard(account.id), connectionsForAccount(account.id)));
  await billingStore.initializeCredits(account.id, binding, credits);
}
const showAddModal = ref(false);
/** One-shot chooser preselection from the `add` deep link; cleared on close. */
const addInitialOptionId = ref<string | null>(null);
// Return context recorded when the add flow was opened from Providers.
const addReturnContext = ref<{ connection: string | null; destination: string | null } | null>(null);
let addFlowCommitted = false;
const showTransfer = ref(false);
const transferMode = ref<"import" | "export">("import");
const showManagedCreate = ref(false);
const showManagedWizard = ref(false);
useLocalizedModalCloseLabel(showManagedCreate, "account-managed-modal");
const editingAccount = ref<Account | null>(null);
const testingAccount = computed(() => (
  testingAccountId.value
    ? accounts.value.find((account) => account.id === testingAccountId.value) ?? null
    : null
));
const managedWizardAccountId = ref<string | null>(null);
const managedDraft = ref({
  name: "",
  username: "",
  inviteUrl: "",
});
const opencodeInviteUrl = ref("");
const browserCapabilities = ref<BrowserCapabilities>({
  mode: "unsupported",
  reason: t("正在检测浏览器能力…"),
});
const openingBrowserTarget = ref<BrowserTarget | null>(null);
const busy = ref(false);
const now = ref(Date.now());
const planFilter = ref<AccountPlanFilter>("all");
const searchFilter = ref("");
const platformSectionRef = ref<InstanceType<typeof PlatformAccountsSection> | null>(null);
const editingPlatformLink = computed(() => (
  editingAccount.value ? platformLinkForAccount(editingAccount.value.id) : null
));
const editingEndpointLockHint = computed(() => {
  const link = editingPlatformLink.value;
  if (!link) return "";
  const parentName = editingAccount.value ? platformParentForAccount(editingAccount.value.id)?.name : null;
  return parentName
    ? t("已关联平台账号 {name}，Endpoint 由平台托管", { name: parentName })
    : t("已关联平台账号，Endpoint 由平台托管");
});
const OLLAMA_WEBSITE_URL = "https://ollama.com";

const statusFilter = ref<AccountStatusFilter>("all");
// Card collapse persists across reloads (localStorage); sort mode stays
// in-memory. Sort mode bypasses the plan/status filters (dragging operates on
// the full layout, which the routing-cards replacement write requires) and
// restores the previous filter and collapse values untouched on exit.
const COLLAPSED_CARDS_STORAGE_KEY = "ocg-manager.accounts-collapsed-cards";
function readCollapsedCardIds(): ReadonlySet<string> {
  try {
    const raw = window.localStorage.getItem(COLLAPSED_CARDS_STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((id): id is string => typeof id === "string"));
  } catch {
    return new Set();
  }
}
const collapsedCardIds = ref<ReadonlySet<string>>(readCollapsedCardIds());
watch(collapsedCardIds, (ids) => {
  try {
    window.localStorage.setItem(COLLAPSED_CARDS_STORAGE_KEY, JSON.stringify([...ids]));
  } catch {
    // Private/locked-down browsers can reject persistence; the in-memory state still works.
  }
});
const sortMode = ref(false);
const effectivePlanFilter = computed<AccountPlanFilter>(() => sortMode.value ? "all" : planFilter.value);
const effectiveStatusFilter = computed<AccountStatusFilter>(() => sortMode.value ? "all" : statusFilter.value);
const canEnterSortMode = computed(() => (
  (accountPage.page?.totalCards ?? 0) > 0
  && !busy.value
  && !platformMutating.value
));
async function toggleSortMode(): Promise<void> {
  if (sortMode.value) {
    sortMode.value = false;
    return;
  }
  if (!canEnterSortMode.value) return;
  const session = accountViewSession;
  if (!await prepareLegacyResources(true)) return;
  if (session !== accountViewSession || route.name !== "accounts") return;
  cancelArrangement();
  sortMode.value = true;
}
function toggleCardCollapse(cardId: string): void {
  const next = new Set(collapsedCardIds.value);
  if (next.has(cardId)) next.delete(cardId);
  else next.add(cardId);
  collapsedCardIds.value = next;
}
function sameInputs(a: readonly unknown[], b: readonly unknown[]): boolean {
  return a.length === b.length && a.every((value, index) => Object.is(value, b[index]));
}

/**
 * Per-key memo for row/card props. A re-render triggered by an unrelated
 * commit (clock tick, billing slot write, another row's usage) reuses the
 * cached object while every input reference is unchanged, so the row
 * component sees identical props and skips its own update.
 */
function createKeyedMemo<K>() {
  const cache = new Map<K, { inputs: readonly unknown[]; value: unknown }>();
  return {
    get<V>(key: K, inputs: readonly unknown[], build: () => V): V {
      const hit = cache.get(key);
      if (hit && sameInputs(hit.inputs, inputs)) return hit.value as V;
      const value = build();
      cache.set(key, { inputs, value });
      return value;
    },
    prune(keys: ReadonlySet<K>): void {
      for (const key of Array.from(cache.keys())) if (!keys.has(key)) cache.delete(key);
    },
  };
}

const cardSummaryMemo = createKeyedMemo<string>();
const cardLinksMemo = createKeyedMemo<string>();
/** Stable per-parent link list; `linksFor` filters a fresh array on every call. */
function cardLinksFor(parent: PlatformAccount | null) {
  if (!parent) return EMPTY_LINKS;
  const links = platformStore.links;
  return cardLinksMemo.get(parent.id, [links], () => platformStore.linksFor(parent.id));
}
const EMPTY_LINKS: PlatformLink[] = [];
function cardSummaryFor(group: DestinationGroup): { total: number; enabled: number } {
  const byId = accountsStore.byId;
  return cardSummaryMemo.get(group.id, [group.credentials, byId], () => {
    let enabled = 0;
    for (const credential of group.credentials) {
      const account = overlayAccountForCredential(credential, byId);
      if (withAccountEnablement(credential, account?.enabled).enabled) enabled += 1;
    }
    return { total: group.credentials.length, enabled };
  });
}
const cardPositions = computed(() => new Map(
  destinationsStore.cards.map((card, index) => [card.id, index] as const)),
);
// The catalog is store-owned so preset-brand prefetch and cross-page caching
// apply; this view only tracks its own loading/error surface.
const providerCatalog = computed(() => providersStore.catalog);
const catalogLoading = ref(false);
const catalogError = ref("");
const platformMutating = computed(() => platformStore.mutating);

const {
  usageLimitsFor,
  usageFor,
  providerUsageFor,
  usageLoadingFor,
  usageLoadErrorFor,
  usageRefreshLoadingFor,
  usageEdits,
  focusUsageEditor,
  updateUsageDraft,
  updateResetsFirstField,
  updateResetsSecondField,
  saveUsage,
  refreshAccountUsage,
  loadAccountUsage,
  forgetAccount,
} = useAccountUsage(accounts, now, providerCatalog, {
  quotaOnly: true,
  endpointUrlFor: (account) => accountInferenceEndpointUrl(
    account,
    identityForCard(account.id),
    connectionsForAccount(account.id),
  ),
  officialBalanceFor: (account) => officialBalanceSupported(
    accountInferenceEndpointUrl(account, identityForCard(account.id), connectionsForAccount(account.id)),
    connectionsForAccount(account.id),
  ),
  afterUsageRefresh: (accountId, isCurrent) => refreshCompanionCatalog(accountId, isCurrent),
  calibrationPlanFor: (accountId) => destinationsStore.destinationForAccount(accountId)?.plan ?? null,
});

// The saved routing-card snapshot is the only ordering source; the visible
// card order then row order is the persisted routing priority. The view keeps
// only a UI-local draft layout while an arrangement is being composed.
const layoutDraft = ref<{ id: string; destinationId: string; credentialIds: string[] }[] | null>(null);
const knownAccountIds = computed(() => new Set(accountsStore.byId.keys()));

/** Draft layout (while arranging) or the committed snapshot. */
const activeCardLayout = computed(() => (
  layoutDraft.value ?? destinationsStore.cards.map((card) => ({
    id: card.id,
    destinationId: card.destination_id,
    credentialIds: [...card.credential_ids],
  }))
));

const allGroups = computed(() => buildRoutingCardGroups(
  activeCardLayout.value.map((card) => ({
    id: card.id,
    destination_id: card.destinationId,
    credential_ids: card.credentialIds,
  })),
  destinationsStore.destinations,
  destinationsStore.credentials,
));

const committedCardLayout = computed(() => destinationsStore.cards.map((card) => ({
  id: card.id,
  destinationId: card.destination_id,
  credentialIds: [...card.credential_ids],
})));

const arrangementBlocked = computed(() => busy.value || platformMutating.value
  || effectivePlanFilter.value !== "all" || effectiveStatusFilter.value !== "all" || !destinationsStore.loaded);
async function saveCardLayout(layout: { id: string; destinationId: string; credentialIds: string[] }[], revision: MutationExpectation): Promise<void> {
  await destinationsStore.replaceRoutingCardLayout(layout, revision);
}
const {
  orderSaving, orderAnnouncement, draggingCardId, draggingCredentialId,
  startCardDrag, startCredentialDrag, handleCardKeydown, applyLayoutChange,
  cancelArrangement, revertActiveArrangement,
} = useRoutingCardLayout({
  committedLayout: committedCardLayout,
  revision: computed(() => destinationsStore.expectation),
  draft: layoutDraft,
  busy: arrangementBlocked,
  message,
  save: saveCardLayout,
  refreshConflict: () => Promise.all([accountsStore.loadPresented(), identitiesStore.loadPresented(), providersStore.loadConnections()]),
});
const arrangementEnabled = computed(() => !arrangementBlocked.value && !orderSaving.value);
/** Calm position-only FLIP for card/row reorders; the dragged element itself snaps. */
const arrangementLayoutTransition = { layout: { duration: 0.18, ease: "easeOut" } };
// Layout FLIP measurement only pays off while arranging: outside sort mode
// the wrappers render as plain divs, so a store commit or clock tick never
// triggers a motion measurement pass over the whole list.
const cardWrapperComponent = computed(() => (sortMode.value ? motion.div : "div"));
function cardWrapperProps(cardId: string): Record<string, unknown> {
  return sortMode.value
    ? { layout: draggingCardId.value === cardId ? false : "position", transition: arrangementLayoutTransition }
    : {};
}
function rowWrapperProps(credentialId: string): Record<string, unknown> {
  return sortMode.value
    ? { layout: draggingCredentialId.value === credentialId ? false : "position", transition: arrangementLayoutTransition }
    : {};
}
const arrangementDisabledHint = computed(() => effectivePlanFilter.value !== "all" || effectiveStatusFilter.value !== "all"
  ? t("清除筛选后可调整顺序") : "");
const removableEmptyCardIds = computed(() => new Set(destinationsStore.cards
  .filter(card => card.credential_ids.length === 0 && destinationsStore.cards.some(other => other.id !== card.id && other.destination_id === card.destination_id))
  .map(card => card.id)));
function draftCards(cards: typeof destinationsStore.cards) {
  return cards.map(card => ({ id: card.id, destinationId: card.destination_id, credentialIds: [...card.credential_ids] }));
}
async function addCardAfter(cardId: string) {
  if (!arrangementEnabled.value) return;
  const card = destinationsStore.cards.find(card => card.id === cardId);
  if (card) await applyLayoutChange(draftCards(addEmptyCardAfter(destinationsStore.cards, card.destination_id, cardId)));
}
async function removeEmptyCardById(cardId: string) {
  if (!arrangementEnabled.value) return;
  const next = removeEmptyCard(destinationsStore.cards, cardId);
  if (next) await applyLayoutChange(draftCards(next));
}
async function handleCardMove(cardId: string, move: RoutingCardMove) {
  if (!arrangementEnabled.value) return;
  const next = moveCardInLayout(destinationsStore.cards, cardId, move);
  if (next) await applyLayoutChange(draftCards(next));
}
const moveToCardState = ref<string | null>(null);
const moveToCardTarget = ref<string | null>(null);
const movingCredential = computed(() => destinationsStore.credentials.find(row => row.id === moveToCardState.value));
const moveToCardSubject = computed(() => movingCredential.value?.name ?? "");
const moveToCardOptions = computed(() => {
  const credential = movingCredential.value;
  if (!credential) return [];
  const options = destinationsStore.cards.flatMap((card, index) => {
    if (card.destination_id !== credential.destination_id || card.credential_ids.includes(credential.id)) return [];
    const names = card.credential_ids.map(id => destinationsStore.credentials.find(row => row.id === id)?.name ?? "").filter(Boolean).join("、");
    return [{ value: card.id, label: t("第 {position} 张 · {names}", { position: index + 1, names: names || t("空卡片") }) }];
  });
  return [...options, { value: "new", label: t("新卡片") }];
});
function setMoveToCardVisible(show: boolean) {
  if (!show && !orderSaving.value) { moveToCardState.value = null; moveToCardTarget.value = null; }
}
function openMoveToCard(accountId: string) {
  if (!arrangementEnabled.value) return;
  const credential = destinationsStore.credentialsByLegacyAccountId.get(accountId);
  if (!credential) return;
  moveToCardState.value = credential.id;
  moveToCardTarget.value = moveToCardOptions.value[0]?.value ?? "new";
}
async function confirmMoveToCard() {
  const credential = movingCredential.value;
  if (!arrangementEnabled.value || !credential || !moveToCardTarget.value) return;
  let cards = destinationsStore.cards;
  let target = moveToCardTarget.value;
  if (target === "new") {
    const sourceIndex = cards.findIndex(card => card.credential_ids.includes(credential.id));
    if (sourceIndex < 0) return;
    target = newRoutingCardId();
    cards = [...cards];
    cards.splice(sourceIndex + 1, 0, { id: target, destination_id: credential.destination_id, credential_ids: [] });
  }
  const next = moveCredentialToCard(cards, credential.id, target);
  if (next && await applyLayoutChange(draftCards(next))) setMoveToCardVisible(false);
}
function handleRowKeydown(event: KeyboardEvent, accountId: string) {
  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
  event.preventDefault();
  void moveWithinDisplayedGroup(accountId, event.key === "ArrowUp" ? -1 : 1);
}
async function addKeyForCard(group: DestinationGroup, parent: PlatformAccount | null) {
  if (parent) { platformSectionRef.value?.openAddKey(parent); return; }
  const source = destinationsStore.credentials.find(row => row.destination_id === group.destination.id);
  const account = source ? overlayAccountForCredential(source, accountsStore.byId) : null;
  if (account) { await openCreateModal(account.id); createModalCardId.value = group.id; return; }
  await providersStore.loadConnections().catch(() => undefined);
  const connection = connectionForDestination(providersStore.connections ?? [], group.destination);
  if (!connection) return;
  editingAccount.value = null;
  addInitialOptionId.value = `connection:${connection.id}`;
  showAddModal.value = true;
}

const managedWizardAccount = computed(() => (
  accounts.value.find(({ id }) => id === managedWizardAccountId.value) ?? null
));
const managedRegistrationAvailable = computed(() => (
  browserCapabilities.value.mode !== "unsupported"
));
const managedRegistrationReason = computed(() => {
  if (browserCapabilities.value.mode === "unsupported") {
    return browserCapabilities.value.reason || t("当前环境不支持独立浏览器");
  }
  return "";
});
const managedInvitePreview = computed(() => {
  try {
    const normalized = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
    return {
      status: undefined as "error" | undefined,
      feedback: normalized
        ? t("用于打开邀请页；与 OpenCode Go 供应商中的值不同时写回。")
        : t("必填。仅接受 opencode.ai 官方 HTTPS 链接。"),
      normalized,
    };
  } catch (error) {
    return {
      status: "error" as const,
      feedback: error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"),
      normalized: "",
    };
  }
});
const managedInviteStatus = computed(() => managedInvitePreview.value.status);
const managedInviteFeedback = computed(() => managedInvitePreview.value.feedback);
const canCreateManagedDraft = computed(() => (
  Boolean(managedDraft.value.name.trim())
  && Boolean(managedInvitePreview.value.normalized)
  && !managedInvitePreview.value.status
));

let previousVisibleIds = new Set<string>();
let previousStatusSignature = "";
const visibleAccountIds = computed(() => {
  // The filter selection has to be part of the memo key: clicking a plan tab
  // or status chip changes no account's status signature, so a key built only
  // from account status would return the previous filter's id set.
  const signature = `${effectivePlanFilter.value}|${effectiveStatusFilter.value}\n` + accounts.value.map((account) => (
    `${account.id}:${accountStatusKey(account, now.value, providerCatalog.value, destinationsStore.destinationForAccount(account.id))}`
  )).join("\n");
  const ids = new Set(
  filterAccounts(
    accounts.value,
    effectivePlanFilter.value,
    effectiveStatusFilter.value,
    now.value,
    providerCatalog.value,
    destinationsStore.destinationForAccount,
  ).map((account) => account.id));
  if (signature === previousStatusSignature) return previousVisibleIds;
  previousStatusSignature = signature;
  previousVisibleIds = ids;
  return ids;
});

const displayedGroups = computed(() => {
  const visibleIds = visibleAccountIds.value;
  const knownIds = knownAccountIds.value;
  const unfiltered = effectivePlanFilter.value === "all" && effectiveStatusFilter.value === "all";
  return allGroups.value.filter((group) => {
    if (group.credentials.length === 0) {
      if (isVacatedCustomShell(group, destinationsStore.credentials)) return false;
      return unfiltered;
    }
    if (group.destination.legacy.kind === "platform_parent") {
      if (group.credentials.length === 0) return unfiltered;
      if (effectivePlanFilter.value !== "all" && effectivePlanFilter.value !== "custom") return false;
      return group.credentials.some((credential) => (
        includeCredentialRow(credential, visibleIds, knownIds)
      ));
    }
    if (isSingleAccountGroup(group)) {
      const credential = group.credentials[0];
      return credential ? includeCredentialRow(credential, visibleIds, knownIds) : false;
    }
    return group.credentials.some((credential) => (
      includeCredentialRow(credential, visibleIds, knownIds)
    ));
  });
});

interface DisplayedGroupView {
  group: DestinationGroup;
  displayGroup: DestinationGroup;
  parent: PlatformAccount | null;
}

const displayedGroupViews = computed((): DisplayedGroupView[] => {
  if (!sortMode.value) return [];
  const views: DisplayedGroupView[] = [];
  const visibleIds = visibleAccountIds.value;
  for (const group of displayedGroups.value) {
    const cards = group.credentials.filter((credential) => (
      includeCredentialRow(credential, visibleIds, knownAccountIds.value)
    ));
    const displayGroup = cards.length === group.credentials.length
      ? group
      : { ...group, credentials: cards };
    if (group.destination.legacy.kind === "platform_parent") {
      const parent = platformStore.parents.find((row) => row.id === group.destination.legacy.id) ?? null;
      if (!parent) continue;
      views.push({ group, displayGroup, parent });
      continue;
    }
    views.push({ group, displayGroup, parent: null });
  }
  return views;
});

let previousPlatformRefreshing: Record<string, boolean> = {};
const platformRefreshing = computed(() => {
  const refreshing = { ...platformStore.refreshing };
  for (const id of Object.keys(refreshStates.value)) {
    if (id.startsWith("platform:")) refreshing[id.slice("platform:".length)] = true;
    else {
      const link = platformStore.linkForAccount(id);
      if (link) refreshing[`${link.platformAccountId}:${id}`] = true;
    }
  }
  const previousKeys = Object.keys(previousPlatformRefreshing);
  const nextKeys = Object.keys(refreshing);
  if (previousKeys.length === nextKeys.length && nextKeys.every((key) => previousPlatformRefreshing[key] === refreshing[key])) {
    return previousPlatformRefreshing;
  }
  previousPlatformRefreshing = refreshing;
  return refreshing;
});
const platformPendingLink = computed(() => platformStore.pendingLink);

// Drop memo entries for cards/rows that left the projection so the caches
// cannot grow across deletions and imports.
watch(displayedGroupViews, (views) => {
  const cardIds = new Set<string>();
  const credentialIds = new Set<string>();
  const parentIds = new Set<string>();
  for (const view of views) {
    cardIds.add(view.group.id);
    if (view.parent) parentIds.add(view.parent.id);
    for (const credential of view.displayGroup.credentials) credentialIds.add(credential.id);
  }
  cardSummaryMemo.prune(cardIds);
  cardLinksMemo.prune(parentIds);
  rowBindingsMemo.prune(credentialIds);
  rowMenuMemo.prune(credentialIds);
});

const planFilterOptions = computed(() => [
  { value: "all", label: t("全部方案") },
  ...(accountPage.page?.planOptions ?? []).map(plan => ({ value: plan.value, label: plan.label })),
]);

const statusFilterOptions = computed(() => [
  { value: "all", label: t("全部状态") },
  { value: "available", label: t("已启用") },
  { value: "cooling", label: t("冷却中") },
  { value: "auth-error", label: t("不可用") },
  { value: "disabled", label: t("已禁用") },
  { value: "registering", label: t("注册中") },
]);



const credentialModalOperations = computed(() => (
  credentialModalAccountId.value ? accountPage.details.get(credentialModalAccountId.value)?.operations : null
));
const credentialModalSupport = computed(() => {
  const detail = credentialModalAccountId.value ? accountPage.details.get(credentialModalAccountId.value) : null;
  return detail ? credentialWriteSupport(detail.operations, detail.identity) : null;
});
const credentialModalBinding = computed(() => credentialModalSupport.value?.bindingRecord ?? null);
const credentialModalConnection = computed(() => (
  credentialModalAccountId.value ? accountPage.details.get(credentialModalAccountId.value)?.connection ?? null : null
));
const credentialModalUnsupported = computed((): MessageKey | null => {
  if (!showCredentialModal.value) return null;
  const support = credentialModalSupport.value;
  if (!support) return "无法确定当前卡片的凭据";
  if (credentialModalMode.value === "rotate" && !support.rotate) return support.unsupportedReason;
  if (credentialModalMode.value === "binding" && !support.binding) return support.unsupportedReason;
  return null;
});

const createModalSupport = computed(() => {
  const detail = createModalAccountId.value ? accountPage.details.get(createModalAccountId.value) : null;
  return detail ? credentialWriteSupport(detail.operations, detail.identity) : null;
});
const createModalUnsupported = computed((): MessageKey | null => {
  if (!showCreateModal.value) return null;
  const support = createModalSupport.value;
  if (!support?.create) return support?.unsupportedReason ?? "无法确定当前卡片的凭据";
  return null;
});
const createModalConnectionId = computed(() => (
  createModalSupport.value?.bindingRecord?.connection_id ?? ""
));
const createModalConnections = computed(() => (
  createModalAccountId.value ? accountPage.details.get(createModalAccountId.value)?.operations.allowedConnections ?? [] : []
));
const createModalShareTargets = computed(() => (
  createModalAccountId.value ? accountPage.details.get(createModalAccountId.value)?.operations.shareTargets ?? [] : []
));

function cardMenuOptions(account: Account) {
  const base = accountMenuOptions(account, now.value, providerCatalog.value, destinationForAccountId(account.id));
  const detail = accountPage.details.get(account.id);
  const extra = accountCredentialMenuOptions(account, detail?.operations, detail?.identity ?? null);
  if (extra.length === 0) return base;
  const editAt = base.findIndex((option) => option.key === "edit");
  if (editAt < 0) return [...base, ...extra];
  return [...base.slice(0, editAt + 1), ...extra, ...base.slice(editAt + 1)];
}

function overlayAccountsFor(group: DestinationGroup): Account[] {
  return group.credentials
    .map((credential) => overlayAccountForCredential(credential, accountsStore.byId))
    .filter((account): account is Account => Boolean(account));
}

function cpaStatusFor(group: DestinationGroup): CpaCardStatus | null {
  return group.destination.adapter === "cpa" ? cpaStore.cardStatus : null;
}

const EMPTY_ROW_LIMITS: UsageLimitView[] = [];
const rowBindingsMemo = createKeyedMemo<string>();
/**
 * Row props bundle, memoized per credential. `now` is deliberately not part
 * of the bundle — it is passed as its own prop so a clock tick does not
 * rebuild every row's bindings, and the memo inputs stay tick-independent.
 */
function credentialRowBindingsFor(credential: DestinationCredential, destination: Destination) {
  const account = overlayAccountForCredential(credential, accountsStore.byId) ?? null;
  const overlayId = account?.id ?? credential.legacy_account_id;
  const usage = usageFor(overlayId);
  const providerUsage = providerUsageFor(overlayId);
  return rowBindingsMemo.get(credential.id, [
    credential,
    destination,
    account,
    identityForCard(overlayId),
    providerCatalog.value,
    usage,
    providerUsage,
    usageEdits.value[overlayId] ?? null,
    usageLoadingFor(overlayId),
    usageLoadErrorFor(overlayId),
    usageRefreshLoadingFor(overlayId),
    !!purchaseDateSaving.value[overlayId],
    busy.value,
    accountNamesById.value,
    providersStore.connections ?? null,
  ], () => ({
    credential,
    destination,
    account,
    identity: identityForCard(overlayId),
    catalog: providerCatalog.value,
    usage,
    providerUsage,
    limits: computed(() => account ? usageLimitsFor(account) : EMPTY_ROW_LIMITS),
    edits: usageEdits.value[overlayId],
    usageLoading: usageLoadingFor(overlayId),
    usageLoadError: usageLoadErrorFor(overlayId),
    usageRefreshLoading: usageRefreshLoadingFor(overlayId),
    purchaseDateSaving: !!purchaseDateSaving.value[overlayId],
    accountNames: accountNamesById.value,
    connections: providersStore.connections,
  }));
}

function rowMenuOptions(
  group: DestinationGroup,
  credential: DestinationCredential,
  index: number,
  parent: PlatformAccount | null,
): AccountMenuOption[] {
  const at = group.credentials.findIndex((row) => row.id === credential.id);
  const resolved = at >= 0 ? at : index;
  const overlay = overlayAccountForCredential(credential, accountsStore.byId);
  const menuTarget = overlay ?? {
    id: credential.legacy_account_id,
    name: credential.name,
  };
  const moves = groupMoveMenuOptions(menuTarget, resolved, group.credentials.length, { canMoveToCard: true })
    .map(option => ({ ...option, disabled: !arrangementEnabled.value || Boolean(option.disabled) }));
  const utilities: AccountMenuOption[] = [];
  if (overlay) {
    utilities.push({
      key: "refresh-usage",
      label: t("刷新"),
      accountId: menuTarget.id,
      accountName: menuTarget.name,
      disabled: parent
        ? platformMutating.value || busy.value || !!platformRefreshing.value[`${parent.id}:${credential.legacy_account_id}`]
        : busy.value || usageRefreshLoadingFor(overlay.id).value,
    });
    if (!parent && usageCompanionCatalog({
      providerId: overlay.provider_id,
      catalog: providerCatalog.value,
      destination: destinationForAccountId(overlay.id),
    }).kind !== "none") {
      utilities.push({
        key: "refresh-models",
        label: t("刷新模型目录"),
        accountId: menuTarget.id,
        accountName: menuTarget.name,
        disabled: busy.value || !accountIsReady(overlay) || Boolean(refreshStates.value[overlay.id]),
      });
    }
    if (accountCapabilities(overlay, providerCatalog.value, destinationForAccountId(overlay.id)).testable) {
      utilities.push({
        key: "test-connection",
        label: t("测试连接"),
        accountId: menuTarget.id,
        accountName: menuTarget.name,
        disabled: busy.value || !accountIsReady(overlay),
      });
    }
  }
  if (group.destination.legacy.kind === "platform_parent") {
    const blocked = platformMutating.value || busy.value;
    return [
      ...utilities,
      { key: "fetch-models", accountId: menuTarget.id, accountName: menuTarget.name, disabled: blocked },
      ...moves.map((option) => ({ ...option, disabled: blocked || Boolean(option.disabled) })),
      { key: "edit-key", accountId: menuTarget.id, accountName: menuTarget.name, disabled: blocked },
      { key: "unlink", accountId: menuTarget.id, accountName: menuTarget.name, disabled: blocked },
    ];
  }
  return overlay ? [...utilities, ...cardMenuOptions(overlay), ...moves] : [...utilities, ...moves];
}

const rowMenuMemo = createKeyedMemo<string>();
/** Memoized per credential; `now` is an input because cooling state gates menu items. */
function rowMenuOptionsFor(
  group: DestinationGroup,
  credential: DestinationCredential,
  index: number,
  parent: PlatformAccount | null,
): AccountMenuOption[] {
  const overlay = overlayAccountForCredential(credential, accountsStore.byId) ?? null;
  const overlayId = overlay?.id ?? credential.legacy_account_id;
  return rowMenuMemo.get(credential.id, [
    credential,
    group.credentials,
    index,
    overlay,
    parent,
    arrangementEnabled.value,
    platformMutating.value,
    busy.value,
    parent ? !!platformRefreshing.value[`${parent.id}:${credential.legacy_account_id}`] : false,
    overlay ? usageRefreshLoadingFor(overlay.id).value : false,
    refreshStates.value[overlayId],
    providerCatalog.value,
    identityForCard(overlayId),
    destinationForAccountId(overlayId),
    providersStore.connections ?? null,
    overlay ? accountStatusKey(overlay, now.value, providerCatalog.value, destinationForAccountId(overlay.id)) : "",
  ], () => rowMenuOptions(group, credential, index, parent));
}

function handleMenuSelect(key: string | number, accountId: string, parent: PlatformAccount | null = null) {
  if (busy.value) return;
  if (key === "refresh-usage") {
    queueAccountRefresh(accountId, parent);
    return;
  }
  if (key === "refresh-models") {
    queueAccountModelRefresh(accountId);
    return;
  }
  if (key === "test-connection") {
    openAccountTest(accountId);
    return;
  }
  if (key === "rotate-key") {
    void openCredentialModal(accountId, "rotate");
    return;
  }
  if (key === "edit-binding") {
    void openCredentialModal(accountId, "binding");
    return;
  }
  if (key === "add-key") {
    void openCreateModal(accountId);
    return;
  }
  if (key === "open-cpa") {
    openCpa();
  } else if (key === "open-console") {
    void openAccountBrowser(accountId, "console");
  } else if (key === "open-site") {
    window.open(OLLAMA_WEBSITE_URL, "_blank", "noopener,noreferrer");
  } else if (key === "continue-setup") {
    openManagedWizard(accountId);
  } else if (key === "edit") {
    openEditModal(accountId);
  } else if (key === "reset") {
    resetCooldown(accountId);
  } else if (key === "reset-profile") {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account) return;
    dialog.warning({
      title: t("重置官网登录状态"),
      content: accountIsReady(account)
        ? t("重置账号 {name} 的独立浏览器 Profile？Google 与 OpenCode 登录状态会被清除，但 Key 不受影响。", { name: account.name })
        : t("重置账号 {name} 的独立浏览器 Profile？登录状态会被清除，注册进度回到 Google 账号步骤。", { name: account.name }),
      positiveText: t("重置"),
      negativeText: t("取消"),
      onPositiveClick: () => resetBrowserProfile(accountId),
    });
  } else if (key === "delete") {
    const account = accounts.value.find((item) => item.id === accountId);
    if (!account) return;
    dialog.warning({
      title: t("删除账号"),
      content: t("删除账号 {name}？账号数据、独立浏览器中的 Cookie 和 Profile 都会被删除。", { name: account.name }),
      positiveText: t("删除"),
      negativeText: t("取消"),
      onPositiveClick: () => deleteAccount(accountId),
    });
  } else if (key === "move-to-card") {
    openMoveToCard(accountId);
  } else if (key === "move-up" || key === "move-down") {
    void moveWithinDisplayedGroup(accountId, key === "move-up" ? -1 : 1);
  } else if (key === "fetch-models") {
    fetchPlatformModels(accountId);
  } else if (key === "edit-key") {
    editPlatformKey(accountId);
  } else if (key === "unlink") {
    unlinkPlatformKey(accountId);
  }
}

function openCpa(): void {
  void router.push({ name: "cpa" });
}

// The edit form for a legacy Custom account defers address/protocol/mapping
// edits to the owning Providers connection; Accounts keeps name/notes/Key.
function onEditConnection(): void {
  const account = editingAccount.value;
  showModal.value = false;
  if (!account) return;
  void openCustomConnectionInProviders(account);
}

async function openCustomConnectionInProviders(account: Account): Promise<void> {
  if (!destinationsStore.loaded) {
    await destinationsStore.load().catch(() => undefined);
  }
  const destinationId = legacyCustomAccountDestinationId(
    account.id,
    destinationsStore.credentialsByLegacyAccountId,
    destinationsStore.destinations,
  );
  void router.push(appViewRoute("providers", {
    ...(destinationId ? { destination: destinationId } : {}),
  }));
}

function openAddModal(): void {
  // Add Account owns creation now; a stale edit target would turn the
  // chooser's save payload into an update of the previously edited account.
  editingAccount.value = null;
  addInitialOptionId.value = null;
  addReturnContext.value = null;
  addFlowCommitted = false;
  showAddModal.value = true;
  void prepareLegacyResources().then(ok => ok ? loadRegistrationOptions() : undefined);
}

/**
 * One-shot deep link (Suppliers Custom API row): open Add Account with the
 * requested chooser option preselected. The parameter is deleted before the
 * modal opens so a reload or close never replays it.
 */
function applyAccountAddDeepLink(): void {
  const search = routeQuerySearch("accounts", route.query);
  const link = readAccountAddDeepLink(search);
  if (!link) return;
  const returnLink = readAccountAddReturn(search);
  const query = { ...route.query };
  delete query.add;
  delete query.from;
  if (returnLink) {
    delete query.connection;
    delete query.destination;
  }
  void router.replace({ query });
  editingAccount.value = null;
  addInitialOptionId.value = link.optionId;
  addReturnContext.value = returnLink
    ? { connection: returnLink.connection, destination: returnLink.destination }
    : null;
  addFlowCommitted = false;
  showAddModal.value = true;
  void prepareLegacyResources().then(ok => ok ? loadRegistrationOptions() : undefined);
}

function openTransfer(mode: "import" | "export"): void {
  transferMode.value = mode;
  showTransfer.value = true;
  void prepareLegacyResources();
}

// The modal emits only after a committed import receipt, so the success
// message belongs to the receipt itself; the list reload is an independent
// revalidation whose failure surfaces on its own alert, never as an import
// failure.
function handleAccountsImported(count: number): void {
  message.success(t("节点配置迁移完成：处理 {count} 项账号。", { count }));
  accountPage.noteMutation();
  invalidateManagementPages("accountPage");
  void Promise.allSettled([
    loadAccounts(),
    providersStore.loadConnections(),
  ]);
}

// The chooser's embedded platform form delegates the write to the section so
// validation, CAS conflict recovery, and the card reload stay in one place.
async function handleCreatePlatform(payload: PlatformAccountFormPayload): Promise<void> {
  const created = await platformSectionRef.value?.createPlatform(payload);
  if (created) {
    addFlowCommitted = true;
    showAddModal.value = false;
  }
}

// The atomic preset create already persisted supplier + first account: the
// commit receipt is authoritative, so the chooser closes immediately (marked
// committed before the close watcher runs), a Providers-origin add navigates
// to the receipt connection's Models tab, and the projections refresh
// separately. A slow or failed refresh reports itself and never says the
// confirmed save failed.
function onPresetAccountCommitted(result: {
  connectionId: string;
  credentialId: string | null;
  accountId: string | null;
  replayed: boolean;
  mode: OnboardingIntent;
}): void {
  const context = addReturnContext.value;
  addFlowCommitted = true;
  showAddModal.value = false;
  accountPage.noteMutation();
  invalidateManagementPages("accountPage");
  if (result.mode === "draft") return; // chooser already routed to Providers
  message.success(t("账号已添加"));
  if (context) {
    void router.push(appViewRoute("providers", {
      connection: result.connectionId,
      tab: "models",
    }));
  }
  void revalidateAfterPresetCommit();
}

async function revalidateAfterPresetCommit(): Promise<void> {
  const session = accountViewSession;
  const results = await Promise.allSettled([
    loadAccounts(),
    loadProviderCatalog(),
    providersStore.loadCatalog(),
    providersStore.loadConnections(),
  ]);
  if (session === accountViewSession && results.some((result) => result.status === "rejected" || result.value === false)) {
    message.warning(t("已保存，但列表刷新失败。手动刷新，不要再次提交。"));
  }
}

async function onPresetAccountConflict(): Promise<void> {
  await Promise.allSettled([
    loadAccounts(),
    loadProviderCatalog(),
    providersStore.loadCatalog(),
    providersStore.loadConnections(),
  ]);
}

function resetFilters(): void {
  searchFilter.value = "";
  planFilter.value = "all";
  statusFilter.value = "all";
}

const accountNamesById = computed(() => {
  const names: Record<string, string> = {};
  for (const account of accounts.value) names[account.id] = account.name;
  return names;
});

function identityForCard(accountId: string) {
  const detail = accountPage.details.get(accountId);
  if (detail && detailCoversReceipt(detail, identitiesStore.snapshotExpectation)) return detail.identity;
  return identitiesStore.byAccountId.get(accountId) ?? null;
}

function detailCoversReceipt(detail: AccountDetail, receipt: MutationExpectation | null): boolean {
  return !receipt || receipt.processGeneration !== detail.revision.processGeneration
    || receipt.expectedRevision <= detail.revision.revision;
}

function connectionsForAccount(accountId: string) {
  const detail = accountPage.details.get(accountId);
  const inventory = providersStore.connections ?? [];
  const selected = detail && detailCoversReceipt(detail, providersStore.connectionsExpectation) ? detail.connection : null;
  return selected ? [selected, ...inventory.filter(connection => connection.id !== selected.id)] : inventory;
}

function platformLinkForAccount(accountId: string) {
  const detail = accountPage.details.get(accountId);
  const complete = platformStore.view;
  const receipt = complete ? { expectedRevision: complete.revision, processGeneration: complete.processGeneration } : null;
  return detail && detailCoversReceipt(detail, receipt) ? detail.platformLink : platformStore.linkForAccount(accountId) ?? null;
}

function platformParentForAccount(accountId: string) {
  const detail = accountPage.details.get(accountId);
  const complete = platformStore.view;
  const receipt = complete ? { expectedRevision: complete.revision, processGeneration: complete.processGeneration } : null;
  const link = platformLinkForAccount(accountId);
  const selected = detail && detailCoversReceipt(detail, receipt) ? detail.platform : null;
  return link ? selected ?? platformStore.parents.find(parent => parent.id === link.platformAccountId) ?? null : null;
}

let credentialOperationOpenGeneration = 0;
async function openCredentialModal(accountId: string, mode: CredentialEditorMode): Promise<void> {
  if (busy.value) return;
  const request = ++credentialOperationOpenGeneration;
  const session = accountViewSession;
  try {
    const detail = await accountPage.loadDetail(accountId);
    if (request !== credentialOperationOpenGeneration || session !== accountViewSession || accountPage.details.get(accountId) !== detail) return;
    const support = credentialWriteSupport(detail.operations, detail.identity);
    if (!(mode === "rotate" ? support.rotate : support.binding)) {
      message.warning(t(support.unsupportedReason ?? "无法确定当前卡片的凭据"));
      return;
    }
    credentialModalExpectation.value = { expectedRevision: detail.revision.revision, processGeneration: detail.revision.processGeneration };
    credentialModalAccountId.value = accountId;
    credentialModalMode.value = mode;
    showCredentialModal.value = true;
  } catch (error) {
    if (session === accountViewSession && request === credentialOperationOpenGeneration) {
      message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  }
}

async function openCreateModal(accountId: string): Promise<void> {
  if (busy.value) return;
  const request = ++credentialOperationOpenGeneration;
  const session = accountViewSession;
  try {
    const detail = await accountPage.loadDetail(accountId);
    if (request !== credentialOperationOpenGeneration || session !== accountViewSession || accountPage.details.get(accountId) !== detail) return;
    const support = credentialWriteSupport(detail.operations, detail.identity);
    if (!support.create) {
      message.warning(t(support.unsupportedReason ?? "无法确定当前卡片的凭据"));
      return;
    }
    createModalExpectation.value = { expectedRevision: detail.revision.revision, processGeneration: detail.revision.processGeneration };
    createModalAccountId.value = accountId;
    createModalCardId.value = accountPage.page?.cards.find(card => card.rows.some(row => row.account?.id === accountId))?.cardId ?? null;
    showCreateModal.value = true;
  } catch (error) {
    if (session === accountViewSession && request === credentialOperationOpenGeneration) {
      message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  }
}

function setCredentialModalVisible(show: boolean): void {
  if (!show && busy.value) return;
  showCredentialModal.value = show;
  if (!show) {
    credentialModalAccountId.value = null;
    credentialModalExpectation.value = null;
  }
}

function setCreateModalVisible(show: boolean): void {
  if (!show && busy.value) return;
  showCreateModal.value = show;
  if (!show) {
    createModalAccountId.value = null;
    createModalExpectation.value = null;
    createModalCardId.value = null;
  }
}

async function refreshAccountsAndIdentities(): Promise<void> {
  await Promise.all([accountsStore.loadPresented(), loadIdentitiesOverlay(), destinationsStore.load()]);
}

// Projection reloads after a confirmed credential write run off the page
// lock; a failure reports itself next to the already-committed write. The
// session fence keeps a late failure from posting into a newer session.
async function revalidateAccountsAndIdentities(session: number): Promise<void> {
  try {
    await refreshAccountsAndIdentities();
  } catch (refreshError) {
    if (session !== accountViewSession) return;
    message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(refreshError) }));
  }
}

// The credential receipt already committed; the projection reload and the
// card placement follow off the page lock and report their own failures.
async function revalidateAfterIdentityCredentialCreate(
  credentialId: string,
  targetCardId: string | null,
  session: number,
): Promise<void> {
  try {
    await refreshAccountsAndIdentities();
    if (session !== accountViewSession) return;
    if (targetCardId) {
      const target = destinationsStore.cards.find(card => card.id === targetCardId);
      const next = target && !target.credential_ids.includes(credentialId)
        ? moveCredentialToCard(destinationsStore.cards, credentialId, targetCardId) : null;
      if (next) await destinationsStore.replaceRoutingCardLayout(draftCards(next));
    }
  } catch (refreshError) {
    if (session !== accountViewSession) return;
    message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(refreshError) }));
  }
}

async function recoverCredentialMutationConflict(error: unknown): Promise<boolean> {
  if (!isRevisionConflict(error)) return false;
  const session = accountViewSession;
  let reloaded = await reloadControlPlaneView();
  if (session !== accountViewSession) return true;
  const id = credentialModalAccountId.value ?? createModalAccountId.value;
  if (reloaded && id) {
    try {
      const detail = await accountPage.loadDetail(id);
      if (session !== accountViewSession || accountPage.details.get(id) !== detail) return true;
      const expectation = { expectedRevision: detail.revision.revision, processGeneration: detail.revision.processGeneration };
      credentialModalExpectation.value = expectation;
      createModalExpectation.value = expectation;
    } catch { reloaded = false; }
  }
  if (reloaded) {
    message.warning(t("凭据设置已被其他操作修改；已重新加载最新状态，请重试。"));
  } else {
    message.warning(t("凭据设置已被其他操作修改；未能加载最新状态，请稍后重试。"));
  }
  return true;
}

async function onRotateCredential(payload: { secretInput: string }): Promise<void> {
  if (busy.value) return;
  const support = credentialModalSupport.value;
  const credentialId = support?.credential?.credential.id;
  if (!support?.rotate || !credentialId) {
    message.warning(t(support?.unsupportedReason ?? "无法确定当前卡片的凭据"));
    return;
  }
  const capturedSession = accountViewSession;
  busy.value = true;
  try {
    await identitiesApi.rotateCredential(
      credentialId,
      payload,
      credentialModalExpectation.value ?? undefined,
    );
    if (capturedSession !== accountViewSession) return;
    showCredentialModal.value = false;
    credentialModalAccountId.value = null;
    credentialModalExpectation.value = null;
    accountPage.noteMutation();
    message.success(t("Key 已轮换"));
    void revalidateAccountsAndIdentities(capturedSession);
  } catch (error) {
    if (capturedSession !== accountViewSession) return;
    if (await recoverCredentialMutationConflict(error)) return;
    message.error(t("轮换 Key 失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (capturedSession === accountViewSession) busy.value = false;
  }
}

async function onPatchBinding(payload: BindingPatchInput): Promise<void> {
  if (busy.value) return;
  const support = credentialModalSupport.value;
  const bindingId = support?.bindingRecord?.id;
  if (!support?.binding || !bindingId) {
    message.warning(t(support?.unsupportedReason ?? "无法确定当前卡片的凭据"));
    return;
  }
  const capturedSession = accountViewSession;
  busy.value = true;
  try {
    await identitiesApi.patchBinding(
      bindingId,
      payload,
      credentialModalExpectation.value ?? undefined,
    );
    if (capturedSession !== accountViewSession) return;
    showCredentialModal.value = false;
    credentialModalAccountId.value = null;
    credentialModalExpectation.value = null;
    accountPage.noteMutation();
    message.success(t("绑定已更新"));
    void revalidateAccountsAndIdentities(capturedSession);
  } catch (error) {
    if (capturedSession !== accountViewSession) return;
    if (await recoverCredentialMutationConflict(error)) return;
    message.error(t("更新绑定失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (capturedSession === accountViewSession) busy.value = false;
  }
}

async function onCreateIdentityCredential(payload: IdentityCredentialCreateInput, credits: CreditSetupInput | null): Promise<void> {
  if (busy.value) return;
  const support = createModalSupport.value;
  const identityId = support?.identityId;
  if (!support?.create || !identityId) {
    message.warning(t(support?.unsupportedReason ?? "无法确定当前卡片的凭据"));
    return;
  }
  const capturedSession = accountViewSession;
  busy.value = true;
  try {
    const targetCardId = createModalCardId.value;
    const created = pendingCreditCreate?.created ?? await identitiesApi.createIdentityCredential(
      identityId,
      payload,
      createModalExpectation.value ?? undefined,
    );
    if (capturedSession !== accountViewSession) return;
    if (credits || pendingCreditCreate) {
      pendingCreditCreate ??= { created, credits: credits! };
      createModalRef.value?.noteSaved();
      await refreshAccountsAndIdentities();
      if (capturedSession !== accountViewSession) return;
      const account = accountsStore.byId.get(created.account_id);
      if (!account) throw new Error(t("未找到指定账号，已清除链接参数"));
      await initializeAccountCredits(account, pendingCreditCreate!.credits);
      if (capturedSession !== accountViewSession) return;
    }
    showCreateModal.value = false;
    createModalAccountId.value = null;
    createModalExpectation.value = null;
    createModalCardId.value = null;
    accountPage.noteMutation();
    message.success(t("Key 已添加"));
    void revalidateAfterIdentityCredentialCreate(created.credential_id, targetCardId, capturedSession);
  } catch (error) {
    if (capturedSession !== accountViewSession) return;
    if (pendingCreditCreate) {
      createModalRef.value?.noteSaved();
      message.error(t("保存失败：{error}", { error: dashboardErrorDetail(error) }));
      return;
    }
    createModalRef.value?.noteFailure(error);
    if (await recoverCredentialMutationConflict(error)) return;
    if (isUncertainCreateFailure(error)) {
      message.warning(t("创建结果未知，Key 可能已添加。用相同内容重试，勿修改后提交。"));
    } else {
      message.error(t("添加 Key 失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    if (capturedSession === accountViewSession) busy.value = false;
  }
}

async function loadIdentitiesOverlay(): Promise<void> {
  try {
    await identitiesStore.loadPresented();
    identitiesError.value = "";
  } catch (error) {
    identitiesError.value = dashboardErrorDetail(error);
  }
}

async function moveWithinDisplayedGroup(accountId: string, delta: number): Promise<void> {
  if (!arrangementEnabled.value) return;
  const credential = destinationsStore.credentialsByLegacyAccountId.get(accountId);
  if (!credential) return;
  const card = destinationsStore.cards.find(card => card.credential_ids.includes(credential.id));
  if (!card) return;
  const next = moveCredentialWithinCard(destinationsStore.cards, card.id, credential.id, delta);
  if (next) await applyLayoutChange(draftCards(next));
}

async function retryDestinations(): Promise<void> {
  try {
    await destinationsStore.load();
    destRefreshError.value = null;
    destRefreshErrorDetail.value = "";
  } catch (error) {
    if (destRefreshError.value) {
      destRefreshErrorDetail.value = dashboardErrorDetail(error);
      message.error(t(DESTINATION_PROJECTION_REFRESH_KEYS[destRefreshError.value], {
        error: destRefreshErrorDetail.value,
      }));
    }
  }
}

async function refreshDestinationProjection(
  failureCode: DestinationProjectionRefreshCode = "refresh_failed",
): Promise<boolean> {
  const result = await loadDestinationProjection(() => destinationsStore.refreshAfterMutation());
  if (result.ok) {
    destRefreshError.value = null;
    destRefreshErrorDetail.value = "";
    return true;
  }
  destRefreshError.value = failureCode;
  destRefreshErrorDetail.value = dashboardErrorDetail(result.error);
  return false;
}

function notifyDestinationRefreshFailure(): void {
  if (!destRefreshError.value) return;
  message.error(t(DESTINATION_PROJECTION_REFRESH_KEYS[destRefreshError.value], {
    error: destRefreshErrorDetail.value,
  }));
}

function handlePlatformDestinationRefreshFailure(error: string): void {
  destRefreshError.value = "refresh_failed";
  destRefreshErrorDetail.value = error;
}

async function refreshPlatformChild(parent: PlatformAccount, accountId: string): Promise<void> {
  const link = platformStore.linkForAccount(accountId);
  if (link) await platformSectionRef.value?.refreshChild(parent, link);
}
function confirmDeleteEmptyGroup(destination: Destination): void {
  if (busy.value || groupDeletion.value || !isDestinationDeletable(destination, destinationsStore.credentials)) return;
  const session = billingStore.sessionEpoch;
  const confirmation = dialog.warning({
    title: t("删除账号组"),
    content: t("删除账号组 {name} 及其连接配置和模型映射？", { name: destination.name }),
    positiveText: t("删除"),
    negativeText: t("取消"),
    onPositiveClick: async (): Promise<boolean> => {
      if (session !== billingStore.sessionEpoch) return true;
      if (groupDeletion.value) return false;
      const operation = Symbol(destination.id);
      groupDeletion.value = { id: destination.id, operation };
      confirmation.loading = true;
      try {
        // Re-read the exact target before deletion: the last Key's background
        // revalidation may still be pending, and this write needs current CAS.
        await destinationsStore.load();
        if (session !== billingStore.sessionEpoch) return true;
        const latest = destinationsStore.destinations.find(row => row.id === destination.id);
        if (!latest) return true;
        if (!isDestinationDeletable(latest, destinationsStore.credentials)) {
          message.warning(t("仍有 Key 使用此连接，无法删除"));
          return false;
        }
        await destinationsStore.deleteDestination(destination.id);
        if (session !== billingStore.sessionEpoch) return true;
        message.success(t("账号组已删除"));
        void Promise.all([providersStore.loadConnections(), providersStore.loadCatalog(), providersStore.loadContracts()])
          .catch(() => undefined);
        return true;
      } catch (error) {
        if (session === billingStore.sessionEpoch) {
          message.error(t("删除失败：{error}", { error: dashboardErrorDetail(error) }));
        }
        return false;
      } finally {
        if (groupDeletion.value?.operation === operation) groupDeletion.value = null;
        confirmation.loading = false;
      }
    },
  });
}

async function waitForPlatformRefresh(id: string, parentId: string, current: () => boolean): Promise<boolean> {
  const blocked = () => platformStore.mutating || Object.entries(platformStore.refreshing).some(([key, active]) =>
    active && (key === parentId || key.startsWith(`${parentId}:`)));
  if (blocked()) refreshStates.value[id] = "queued";
  const ready = await waitForAccountRefreshIdle(blocked, () => Boolean(refreshStates.value[id]) && current());
  if (!ready || !refreshStates.value[id] || !current()) return false;
  refreshStates.value[id] = "running";
  return true;
}

function queueAccountRefresh(accountId: string, parent: PlatformAccount | null): void {
  const account = accountsStore.byId.get(accountId);
  if (!account) return;
  const endpoint = destinationForAccountId(accountId);
  const session = billingStore.sessionEpoch;
  const isCurrent = () => sessionStore.authenticated && billingStore.sessionEpoch === session
    && accountsStore.byId.get(accountId)?.updated_at === account.updated_at
    && destinationForAccountId(accountId)?.id === endpoint?.id
    && (parent
      ? platformStore.linkForAccount(accountId)?.platformAccountId === parent.id
        && platformRefreshBinding(platformStore.parents.find(row => row.id === parent.id)) === platformRefreshBinding(parent)
      : !platformStore.linkForAccount(accountId));
  // Unknown snapshots and platform/model writes retain the exclusive lane.
  const sharedQuota = !parent && billingStore.slotFor(accountId).value?.status?.officialRefresh === true;
  void refreshQueue.enqueue(accountId, isCurrent, async current => {
    if (parent) {
      if (!await waitForPlatformRefresh(accountId, parent.id, current)) return;
      await refreshPlatformChild(platformStore.parents.find(row => row.id === parent.id)!, accountId);
    } else {
      // A preceding queued write may have advanced CAS since this row was read.
      await loadAccountUsage(accountId);
      // A capability change must not turn a shared quota job into a model write.
      if (sharedQuota && billingStore.slotFor(accountId).value?.status?.officialRefresh !== true) return;
      if (current()) await refreshAccountUsage(accountId);
    }
  }, { exclusive: !sharedQuota }).then(async () => {
    // Release this account's refresh state and pool slot before projection I/O.
    if (isCurrent()) await destinationsStore.load();
  }).catch(error => {
    if (isCurrent()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function queueAccountModelRefresh(accountId: string): void {
  const account = accountsStore.byId.get(accountId);
  if (!account || platformStore.linkForAccount(accountId)) return;
  const destinationId = destinationForAccountId(accountId)?.id;
  const session = billingStore.sessionEpoch;
  const current = () => sessionStore.authenticated && billingStore.sessionEpoch === session
    && accountsStore.byId.get(accountId)?.updated_at === account.updated_at
    && destinationForAccountId(accountId)?.id === destinationId
    && !platformStore.linkForAccount(accountId);
  void refreshQueue.enqueue(accountId, current, async isCurrent => {
    await destinationsStore.load();
    if (isCurrent()) await refreshCompanionCatalog(accountId, isCurrent);
  }).catch(error => {
    if (current()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function queuePlatformParentRefresh(parent: PlatformAccount): void {
  const session = billingStore.sessionEpoch;
  const current = () => sessionStore.authenticated && billingStore.sessionEpoch === session
    && platformRefreshBinding(platformStore.parents.find(row => row.id === parent.id)) === platformRefreshBinding(parent);
  void refreshQueue.enqueue(`platform:${parent.id}`, current, async isCurrent => {
    if (!await waitForPlatformRefresh(`platform:${parent.id}`, parent.id, isCurrent)) return;
    await platformSectionRef.value?.refreshParent(platformStore.parents.find(row => row.id === parent.id)!);
  }).then(async () => {
    if (current()) await destinationsStore.load();
  }).catch(error => {
    if (current()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function fetchPlatformModels(accountId: string): void {
  const account = accounts.value.find((item) => item.id === accountId);
  if (account) platformSectionRef.value?.fetchModels(account);
}

function fetchAllPlatformModels(keys: Account[]): void {
  platformSectionRef.value?.fetchModelsAll(keys);
}

const platformKeyModelsAccountId = ref<string | null>(null);
const platformKeyModelsAccount = computed(() => (
  platformKeyModelsAccountId.value
    ? accounts.value.find((account) => account.id === platformKeyModelsAccountId.value) ?? null
    : null
));

function openPlatformKeyModels(accountId: string): void {
  platformKeyModelsAccountId.value = accountId;
}

function setPlatformKeyModelsVisible(show: boolean): void {
  if (!show) platformKeyModelsAccountId.value = null;
}

function refreshPlatformKeyModels(): void {
  const account = platformKeyModelsAccount.value;
  if (account) platformSectionRef.value?.fetchModels(account);
}

function editPlatformKey(accountId: string): void {
  const account = accounts.value.find((item) => item.id === accountId);
  if (account) platformSectionRef.value?.openEditKey(account);
}

function unlinkPlatformKey(accountId: string): void {
  const account = accounts.value.find((item) => item.id === accountId);
  const link = platformStore.linkForAccount(accountId);
  if (account && link) platformSectionRef.value?.confirmUnlink(account, link);
}

function openManagedCreateModal(): void {
  if (!managedRegistrationAvailable.value) return;
  showAddModal.value = false;
  managedDraft.value = {
    name: "",
    username: "",
    inviteUrl: opencodeInviteUrl.value || DEFAULT_OPENCODE_INVITE_URL,
  };
  showManagedCreate.value = true;
}

function normalizeManagedInviteDraft(): void {
  try {
    managedDraft.value.inviteUrl = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
  } catch {
    // Keep the raw value so the form can show validation feedback.
  }
}

async function ensureInviteUrlSaved(inviteUrl: string): Promise<void> {
  if (inviteUrl === opencodeInviteUrl.value) return;
  const session = accountViewSession;
  // Partial presentation write through the settings owner. The ack is not a
  // canonical snapshot; ignore it. A session that ended during the await
  // must not update the local cache or continue into credential creation.
  await settingsStore.patchPresented({ opencode_invite_url: inviteUrl });
  if (session !== accountViewSession) return;
  opencodeInviteUrl.value = inviteUrl;
}

function setManagedCreateVisible(show: boolean): void {
  if (!show && busy.value) return;
  showManagedCreate.value = show;
  if (!show) restoreAddOrigin();
}

function setManagedWizardVisible(show: boolean): void {
  if (!show && busy.value) return;
  showManagedWizard.value = show;
  if (!show) restoreAddOrigin();
}

function openManagedWizard(accountId: string): void {
  const account = accounts.value.find(({ id }) => id === accountId);
  if (!account || !isManagedOnboardingAccount(account) || accountIsReady(account)) return;
  managedWizardAccountId.value = accountId;
  showManagedWizard.value = true;
}

function openInviteUrl(): void {
  addReturnContext.value = null;
  showAddModal.value = false;
  void router.push(appViewRoute("providers", {
    provider: DEFAULT_PROVIDER_ID,
    tab: "settings",
  }));
}

function openEditModal(id: string): void {
  editingAccount.value = accounts.value.find((account) => account.id === id) ?? null;
  showModal.value = true;
}

function clearAccountDeepLink(): void {
  if (!("account_id" in route.query)) return;
  const query = { ...route.query };
  delete query.account_id;
  void router.replace({ query });
}

function setAccountFormVisible(show: boolean): void {
  showModal.value = show;
}

watch(showModal, (show) => {
  if (!show) clearAccountDeepLink();
});

function restoreAddOrigin(): void {
  addInitialOptionId.value = null;
  const context = addReturnContext.value;
  addReturnContext.value = null;
  if (context && route.name === "accounts") {
    void router.push(appViewRoute("providers", {
      ...(context.destination ? { destination: context.destination } : {}),
      ...(!context.destination && context.connection ? { connection: context.connection } : {}),
    }));
  }
}

function setAddModalVisible(show: boolean): void {
  if (!show && !addFlowCommitted) restoreAddOrigin();
  showAddModal.value = show;
}

watch(showAddModal, (show) => {
  if (show || showManagedCreate.value) return;
  addInitialOptionId.value = null;
  pendingNewCredits.value = null;
  addReturnContext.value = null;
  addFlowCommitted = false;
});

/** Providers-originated add flows select the committed connection on save. */
function committedAddTarget(
  created: Account,
  optionId: string | null,
  context: { connection: string | null; destination: string | null },
): ProviderScopeQuery {
  if (optionId?.startsWith("connection:")) {
    return { connection: optionId.slice("connection:".length) };
  }
  if (created.provider_id !== "custom") return { provider: created.provider_id };
  if (context.destination) return { destination: context.destination };
  if (context.connection) return { connection: context.connection };
  return {};
}

async function createManagedAccount(): Promise<void> {
  const name = managedDraft.value.name.trim();
  if (!name || busy.value || !managedRegistrationAvailable.value || !canCreateManagedDraft.value) {
    return;
  }
  let inviteUrl = "";
  try {
    inviteUrl = normalizeOpenCodeInviteUrl(managedDraft.value.inviteUrl);
  } catch (error) {
    message.error(error instanceof Error ? t(error.message as MessageKey) : t("邀请链接格式无效"));
    return;
  }
  if (!inviteUrl) {
    message.error(t("填写邀请链接"));
    return;
  }
  managedDraft.value.inviteUrl = inviteUrl;
  // Capture immutable parameters before any await; the draft refs may change.
  const username = managedDraft.value.username.trim();
  const session = accountViewSession;
  busy.value = true;
  try {
    await ensureInviteUrlSaved(inviteUrl);
    if (session !== accountViewSession) return;
    const created = await dashboardApi.createManagedAccount({
      name,
      ...(username ? { username } : {}),
    });
    if (session !== accountViewSession) return;
    addAccount(created);
    message.success(t("注册草稿已创建"));
    showManagedCreate.value = false;
    managedWizardAccountId.value = created.id;
    showManagedWizard.value = true;
    void revalidateAfterManagedCreate(session);
  } catch (error) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(error)) return;
    if (session !== accountViewSession) return;
    message.error(t("创建注册草稿失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (session === accountViewSession) busy.value = false;
  }
}

async function advanceManagedSetup(accountId: string, setupStep: AccountSetupStep): Promise<void> {
  if (busy.value) return;
  const session = accountViewSession;
  busy.value = true;
  try {
    const updated = await dashboardApi.advanceAccountSetup(accountId, setupStep);
    if (session !== accountViewSession) return;
    replaceAccount(updated);
    message.success(t("注册进度已保存"));
  } catch (error) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(error)) return;
    if (session !== accountViewSession) return;
    await recoverManagedSetupConflict(accountId, error);
    if (session !== accountViewSession) return;
    message.error(t("保存注册进度失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (session === accountViewSession) busy.value = false;
  }
}

async function verifyManagedKey(accountId: string, key: string): Promise<void> {
  if (busy.value) return;
  const session = accountViewSession;
  busy.value = true;
  try {
    const updated = await dashboardApi.verifyManagedAccountKey(accountId, key);
    if (session !== accountViewSession) return;
    replaceAccount(updated);
    if (accountIsReady(updated)) {
      showManagedWizard.value = false;
      const context = addReturnContext.value;
      addReturnContext.value = null;
      if (context) void router.push(appViewRoute("providers", committedAddTarget(updated, null, context)));
      message.success(isCooling(updated, now.value)
        ? t("Key 有效，账号已启用并按上游响应进入冷却")
        : t("Key 验证成功，账号已启用"));
      void revalidateAfterManagedKeyReady(updated, session);
    }
  } catch (error) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(error)) return;
    if (session !== accountViewSession) return;
    await recoverManagedSetupConflict(accountId, error);
    if (session !== accountViewSession) return;
    message.error(t("Key 验证失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (session === accountViewSession) busy.value = false;
  }
}

async function openAccountBrowser(accountId: string, target: BrowserTarget): Promise<void> {
  if (openingBrowserTarget.value) return;
  if (browserCapabilities.value.mode === "unsupported") {
    message.error(browserCapabilities.value.reason || t("当前环境不支持独立浏览器"));
    return;
  }
  let remoteTab: Window | null = null;
  if (browserCapabilities.value.mode === "remote") {
    remoteTab = window.open("", "_blank");
    if (!remoteTab) {
      message.error(t("浏览器阻止了新标签页，请允许此站点打开弹出窗口"));
      return;
    }
    remoteTab.opener = null;
  }
  openingBrowserTarget.value = target;
  const session = accountViewSession;
  try {
    const result = await dashboardApi.openAccountBrowser(accountId, target);
    // A late receipt after session teardown only closes the blank tab this
    // call opened; it never navigates or notifies into the new session.
    if (session !== accountViewSession) {
      remoteTab?.close();
      return;
    }
    if (result.mode === "remote") {
      if (!result.session_token) throw new Error(t("服务未返回远程浏览器会话令牌"));
      if (!remoteTab) throw new Error(t("浏览器模式已变化，请重试"));
      remoteTab.location.replace(browserViewUrl(window.location.href, result.session_token));
      message.success(t("远程浏览器已在新标签页打开"));
    } else {
      remoteTab?.close();
      message.success(t("已使用该账号的独立 Profile 打开浏览器"));
    }
  } catch (error) {
    remoteTab?.close();
    if (session !== accountViewSession) return;
    message.error(t("打开浏览器失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (session === accountViewSession) openingBrowserTarget.value = null;
  }
}

async function resetBrowserProfile(accountId: string): Promise<void> {
  const session = accountViewSession;
  try {
    const updated = await dashboardApi.resetAccountBrowserProfile(accountId);
    if (session !== accountViewSession) return;
    replaceAccount(updated);
    if (!accountIsReady(updated)) {
      forgetAccount(accountId);
    }
    message.success(t("官网登录状态已重置"));
  } catch (error) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(error)) return;
    if (session !== accountViewSession) return;
    message.error(t("重置官网登录状态失败：{error}", { error: dashboardErrorDetail(error) }));
  }
}

function replaceAccount(account: Account): void {
  accountsStore.upsertDetailAccount(account);
  revalidatePageAfterWrite();
  if (editingAccount.value?.id === account.id) editingAccount.value = account;
}

function addAccount(account: Account): void {
  accountsStore.upsertDetailAccount(account);
  revalidatePageAfterWrite();
}

async function refreshCatalogIfNewProvider(account: Account, session: number): Promise<void> {
  if (!isFirstReadyProviderAccount(account, accounts.value)) return;
  const surface = findPlanDefinition(account.provider_id, providerCatalog.value);
  if (!surface || surface.dynamic || surface.kind === "custom") return;
  try {
    const contracts = await providersStore.loadContracts();
    if (session !== accountViewSession) return;
    if (!shouldRefreshCatalogForNewProviderAccount(account, accounts.value, contracts)) return;
    await providersStore.refreshContractCatalog("provider", account.provider_id);
    if (session !== accountViewSession) return;
    message.success(t("已刷新模型目录"));
  } catch (error) {
    if (session !== accountViewSession) return;
    message.warning(t("刷新模型目录失败：{error}", { error: dashboardErrorDetail(error) }));
    message.info(`${t("供应商")} → ${t("刷新模型目录")}`);
  }
}

async function revalidateAfterManagedCreate(session: number): Promise<void> {
  const destRefreshed = await refreshDestinationProjection("created_refresh_failed");
  if (session !== accountViewSession) return;
  if (!destRefreshed) notifyDestinationRefreshFailure();
  void providersStore.loadConnections().catch(() => undefined);
}

async function revalidateAfterManagedKeyReady(account: Account, session: number): Promise<void> {
  await loadAccountUsage(account.id);
  if (session !== accountViewSession) return;
  await refreshCatalogIfNewProvider(account, session);
}

const companionCatalogInflight = new Set<string>();

function destinationForAccountId(accountId: string): Destination | null {
  return destinationsStore.destinationForAccount(accountId);
}

async function refreshCompanionCatalog(accountId: string, isCurrent: () => boolean): Promise<void> {
  if (!isCurrent()) return;
  const account = accounts.value.find((item) => item.id === accountId);
  if (!account) return;
  const destination = destinationForAccountId(accountId);
  const companion = usageCompanionCatalog({
    providerId: account.provider_id,
    catalog: providerCatalog.value,
    destination,
  });
  const lockKey = usageCompanionCatalogLockKey(companion, accountId, destination?.id ?? null);
  if (!lockKey || companionCatalogInflight.has(lockKey)) return;
  companionCatalogInflight.add(lockKey);
  try {
    if (companion.kind === "provider_catalog") {
      const contracts = providersStore.contracts ?? await providersStore.loadContracts();
      if (!isCurrent()) return;
      if (!providerContractAllowsCatalogRefresh(contracts, companion.providerId)) return;
      await providersStore.refreshContractCatalog("provider", companion.providerId);
      if (!isCurrent()) return;
      await refreshDestinationProjection();
      if (!isCurrent()) return;
      message.success(t("已刷新模型目录"));
      return;
    }
    const endpointUrl = account.custom_config?.endpoint_url?.trim() || destination?.base_url?.trim() || "";
    const protocol = account.custom_config?.upstream_protocol
      ?? destination?.protocols[0]
      ?? "chat_completions";
    if (!endpointUrl) return;
    const discovery = await dashboardApi.discoverCustomModels({
      endpoint_url: endpointUrl,
      upstream_protocol: protocol,
      account_id: account.id,
    });
    if (!isCurrent()) return;
    if (discovery.models.length === 0) {
      message.warning(t("该 Key 未返回可用模型；确认 Key 与站点地址无误后重试。"));
      return;
    }
    if (companion.kind === "http_destination") {
      if (!destination || !isDestinationEditable(destination)) return;
      const merged = mergeDiscoveredCatalogModels(
        destination.catalog,
        discovery.models,
        protocol,
      );
      const draft = destinationEditDraft({ ...destination, catalog: merged.catalog });
      const plan = planDestinationSave(destination, destinationsStore.credentials, draft);
      if (plan.status === "invalid") {
        message.warning(t(DESTINATION_EDIT_ISSUE_KEYS[plan.issue]));
        return;
      }
      if (plan.status !== "patch") return;
      await destinationsStore.patchDestination(destination.id, plan.input);
      if (!isCurrent()) return;
      if (merged.added === 0) {
        message.success(t("已刷新模型目录"));
        return;
      }
      message.success(
        discovery.truncated
          ? t("已导入 {count} 个模型（列表被截断）", { count: merged.added })
          : t("已导入 {count} 个模型", { count: merged.added }),
      );
      return;
    }
    const merged = mergeDiscoveredAccountCapabilities(
      account.model_capabilities,
      discovery.models,
      protocol,
    );
    if (merged.added === 0) {
      message.success(t("已刷新模型目录"));
      return;
    }
    const updated = await dashboardApi.updateAccountModelCapabilities(account.id, merged.capabilities);
    if (!isCurrent()) return;
    replaceAccount(updated);
    await refreshDestinationProjection();
    if (!isCurrent()) return;
    message.success(
      discovery.truncated
        ? t("已导入 {count} 个模型（列表被截断）", { count: merged.added })
        : t("已导入 {count} 个模型", { count: merged.added }),
    );
  } catch (error) {
    if (!isCurrent()) return;
    message.warning(t("刷新模型目录失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    companionCatalogInflight.delete(lockKey);
  }
}

function removeAccountState(id: string): void {
  accountsStore.removeAccount(id);
  accountPage.removeAccount(id);
  invalidateManagementPages("accountPage");
  void loadAccounts();
  forgetAccount(id);
  if (testingAccountId.value === id) testingAccountId.value = null;
  delete providerSettingsSaving.value[id];
  delete purchaseDateSaving.value[id];
}

function accountHasUsageDisplay(account: Account): boolean {
  const surface = findPlanDefinition(account.provider_id, providerCatalog.value);
  if (surface?.model_source === "official_api_preset") return true;
  if (surface?.usage_availability === "available" || surface?.manual_usage_calibration === true) {
    return true;
  }
  if (officialBalanceSupported(accountInferenceEndpointUrl(
    account,
    identityForCard(account.id),
    connectionsForAccount(account.id),
  ), connectionsForAccount(account.id))) {
    return true;
  }
  if (platformLinkForAccount(account.id)) return false;
  return surface?.kind === "custom" || surface?.dynamic === true;
}

async function refreshAccountState(id: string): Promise<Account | null> {
  const session = accountViewSession;
  const loaded = await accountsStore.loadPresented();
  // A session change during the reload leaves the dead recovery flow silent.
  if (session !== accountViewSession) return null;
  const account = loaded.find((item) => item.id === id);
  if (!account) {
    removeAccountState(id);
    message.warning(t("未找到该账号，已自动刷新列表"));
    return null;
  }
  if (accountIsReady(account) && accountHasUsageDisplay(account)) {
    await loadAccountUsage(id);
  } else {
    forgetAccount(id);
  }
  return account;
}

async function recoverManagedSetupConflict(accountId: string, error: unknown): Promise<void> {
  if (!(error instanceof DashboardRequestError) || ![404, 409].includes(error.status)) return;
  const session = accountViewSession;
  try {
    const account = await refreshAccountState(accountId);
    if (session !== accountViewSession) return;
    if (!account || accountIsReady(account)) {
      showManagedWizard.value = false;
      managedWizardAccountId.value = null;
    }
  } catch {
    // Preserve the original mutation error; the next explicit refresh can retry reconciliation.
  }
}

let accountLoadGeneration = 0;
async function loadAccounts(): Promise<boolean> {
  const generation = ++accountLoadGeneration;
  const session = accountViewSession;
  const current = () => generation === accountLoadGeneration && session === accountViewSession;
  try {
    await accountPage.load(currentPageQuery());
    if (!current()) return false;
    now.value = Date.now();
    void applyPageAccountDeepLink();
    return true;
  } catch (e) {
    if (!current()) return false;
    return false;
  }
}

let usageReadGeneration = 0;
async function loadRegistrationOptions(): Promise<void> {
  const session = accountViewSession;
  const [settingsResult, browserResult] = await Promise.allSettled([
    settingsStore.loadPresented(),
    dashboardApi.getBrowserCapabilities(),
  ]);
  if (session !== accountViewSession) return;
  if (settingsResult.status === "fulfilled") {
    opencodeInviteUrl.value = settingsResult.value.opencode_invite_url || "";
  } else {
    opencodeInviteUrl.value = "";
  }
  if (browserResult.status === "fulfilled") {
    browserCapabilities.value = browserResult.value;
  } else {
    browserCapabilities.value = {
      mode: "unsupported",
      reason: t("浏览器能力检测失败：{error}", { error: dashboardErrorDetail(browserResult.reason) }),
    };
  }
}

async function loadProviderCatalog(): Promise<void> {
  catalogLoading.value = true;
  catalogError.value = "";
  try {
    await providersStore.loadCatalog();
  } catch (e) {
    catalogError.value = dashboardErrorDetail(e);
    // Fail closed: the add modal will fall back to the legacy OpenCode Go flow
    // so the primary creation path keeps working even when the catalog is down.
  } finally {
    catalogLoading.value = false;
  }
}

async function initializeAccounts() {
  await loadAccounts();
}

async function onFormSave(payload: AccountInput | AccountFormPayload) {
  if (busy.value) return;
  const capturedSession = accountViewSession;
  const editing = editingAccount.value;
  if (editing) {
    const update: AccountUpdate = {
      name: payload.name,
      username: payload.username ?? "",
      purchase_date: payload.purchase_date,
      notes: payload.notes ?? "",
    };
    if (payload.key !== undefined) update.key = payload.key;
    if (payload.ollama_billing_tier !== undefined) {
      update.ollama_billing_tier = payload.ollama_billing_tier;
    }
    busy.value = true;
    try {
      const saved = pendingCreditEdit?.account ?? await dashboardApi.updateAccount(editing.id, update);
      if (capturedSession !== accountViewSession) return;
      replaceAccount(saved);
      const credits = (payload as AccountFormPayload).credits;
      if (credits || pendingCreditEdit) {
        pendingCreditEdit ??= { account: saved, credits: credits! };
        accountFormRef.value?.noteSaved();
        await nextTick();
        if (capturedSession !== accountViewSession) return;
        await initializeAccountCredits(saved, pendingCreditEdit!.credits);
        if (capturedSession !== accountViewSession) return;
      }
      // The confirmed receipt ends the save: commit, close, and release the
      // page. Projection/usage reads follow in the background and report
      // their own failures; a slow read never says the write failed.
      message.success(t("账号已更新"));
      showModal.value = false;
      void revalidateAfterAccountSave(saved, capturedSession, "updated");
    } catch (e) {
      if (capturedSession !== accountViewSession) return;
      if (pendingCreditEdit) {
        accountFormRef.value?.noteSaved();
        message.error(t("保存失败：{error}", { error: dashboardErrorDetail(e) }));
        return;
      }
      if (await recoverAccountMutationConflict(e)) return;
      message.error(t("保存失败：{error}", { error: dashboardErrorDetail(e) }));
    } finally {
      if (capturedSession === accountViewSession) busy.value = false;
    }
  } else {
    const { credits, ...accountPayload } = payload as AccountFormPayload;
    const input = accountCreateRequestInput(accountPayload as AccountInput);
    busy.value = true;
    try {
      const created = pendingNewCredits.value?.account ?? await dashboardApi.createAccount(input);
      if (capturedSession !== accountViewSession) return;
      addAccount(created);
      if (credits || pendingNewCredits.value) {
        pendingNewCredits.value ??= { account: created, credits: credits! };
        await nextTick();
        if (capturedSession !== accountViewSession) return;
        await initializeAccountCredits(created, pendingNewCredits.value.credits);
        if (capturedSession !== accountViewSession) return;
      }
      message.success(t("账号已添加"));
      // Create payloads arrive from the Add Account chooser's embedded form;
      // only a successful create closes it, so a failed save keeps the draft.
      const returnContext = addReturnContext.value;
      const returnOptionId = addInitialOptionId.value;
      addFlowCommitted = true;
      showModal.value = false;
      showAddModal.value = false;
      if (returnContext) {
        void router.push(appViewRoute("providers", committedAddTarget(created, returnOptionId, returnContext)));
      }
      // Go uses official usage; GOAT and Ollama project locally priced OCG request logs.
      void revalidateAfterAccountSave(created, capturedSession, "created");
    } catch (e) {
      if (capturedSession !== accountViewSession) return;
      if (pendingNewCredits.value) {
        message.error(t("Key 已保存，请重试额度初始化。"));
        return;
      }
      if (await recoverAccountMutationConflict(e)) return;
      message.error(t("保存失败：{error}", { error: dashboardErrorDetail(e) }));
    } finally {
      if (capturedSession === accountViewSession) busy.value = false;
    }
  }
}

// Follow-up reads after a confirmed account save. They run off the page
// lock: last-good content stays rendered, each failure reports itself, and
// nothing here rethrows into the already-completed write.
async function revalidateAfterAccountSave(
  account: Account,
  session: number,
  kind: "updated" | "created",
): Promise<void> {
  await runAccountSaveFollowup({
    kind,
    accountId: account.id,
    hasUsageDisplay: accountHasUsageDisplay(account),
    usageReady: accountIsReady(account),
    isCurrent: () => session === accountViewSession,
    refreshProjection: (created) => refreshDestinationProjection(
      created ? "created_refresh_failed" : "refresh_failed",
    ),
    notifyProjectionFailure: notifyDestinationRefreshFailure,
    loadConnections: () => providersStore.loadConnections(),
    refreshCatalogForNewProvider: () => refreshCatalogIfNewProvider(account, session),
    loadUsage: (accountId) => loadAccountUsage(accountId),
  });
}

async function updatePurchaseDate(accountId: string, purchaseDate: string): Promise<void> {
  const account = accounts.value.find((item) => item.id === accountId);
  if (
    !account
    || !accountIsReady(account)
    || !accountCapabilities(account, providerCatalog.value, destinationForAccountId(account.id)).hasExpiry
    || busy.value
    || purchaseDateSaving.value[accountId]
  ) return;

  const session = accountViewSession;
  purchaseDateSaving.value[accountId] = true;
  try {
    const saved = await dashboardApi.updateAccount(accountId, {
      purchase_date: purchaseDate,
    });
    if (session !== accountViewSession) return;
    // The confirmed receipt ends the edit: commit, notify, and the finally
    // releases the row state now. The usage read is a read-only follow-up that
    // billing owns; it never postpones or rewrites the saved result.
    replaceAccount(saved);
    message.success(t("购买日期已更新"));
    if (accountHasUsageDisplay(saved)) void loadAccountUsage(saved.id);
  } catch (error) {
    if (session !== accountViewSession) return;
    if (!(await recoverAccountMutationConflict(error))) {
      if (session !== accountViewSession) return;
      message.error(t("保存失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    if (session === accountViewSession) purchaseDateSaving.value[accountId] = false;
  }
}

function openAccountTest(id: string) {
  if (!accounts.value.some((account) => account.id === id)) return;
  testingAccountId.value = id;
}

function setAccountTestVisible(show: boolean) {
  if (!show) testingAccountId.value = null;
}

async function toggleAccount(id: string) {
  const account = accounts.value.find((item) => item.id === id);
  // The Zen Free singleton only accepts the dedicated provider-settings write;
  // never fall back to the generic account PATCH/toggle for it.
  if (account && accountCapabilities(account, providerCatalog.value, destinationForAccountId(account.id)).toggleWrite === "provider_settings") {
    await saveZenProviderSettings(account, !account.enabled);
    return;
  }
  const session = accountViewSession;
  try {
    const updated = await dashboardApi.toggleAccount(id);
    if (session !== accountViewSession) return;
    replaceAccount(updated);
    void refreshDestinationProjection().then(ok => { if (session === accountViewSession && !ok) notifyDestinationRefreshFailure(); });
  } catch (e) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(e)) return;
    if (session !== accountViewSession) return;
    message.error(t("切换失败：{error}", { error: dashboardErrorDetail(e) }));
  }
}

async function reloadControlPlaneView(): Promise<boolean> {
  const session = accountViewSession;
  const knownIds = new Set(accounts.value.map(({ id }) => id));
  let loaded: Account[];
  try {
    loaded = await accountsStore.loadPresented();
  } catch {
    return false;
  }
  // A session change during the reload must not let the recovery write
  // removals, modal state, or editing targets into the cleared/new session.
  if (session !== accountViewSession) return false;

  const loadedIds = new Set(loaded.map(({ id }) => id));
  for (const id of knownIds) {
    if (!loadedIds.has(id)) removeAccountState(id);
  }
  // Only the CAS-tracked projections are invalidated by an account/credential
  // conflict; provider connections are not, so they keep their cached list.
  await Promise.allSettled([
    loadIdentitiesOverlay(),
    destinationsStore.load(),
  ]);
  if (session !== accountViewSession) return false;
  if (editingAccount.value) {
    const stillListed = reconcileEditingAccount(loaded, editingAccount.value.id);
    editingAccount.value = stillListed;
    // The form derives edit-vs-create from account presence; without closing
    // the modal it would morph into the create form for a deleted account.
    if (!stillListed) showModal.value = false;
  }
  if (managedWizardAccountId.value && !loadedIds.has(managedWizardAccountId.value)) {
    showManagedWizard.value = false;
    managedWizardAccountId.value = null;
  }
  if (credentialModalAccountId.value && !loadedIds.has(credentialModalAccountId.value)) {
    showCredentialModal.value = false;
    credentialModalAccountId.value = null;
    credentialModalExpectation.value = null;
  }
  if (createModalAccountId.value && !loadedIds.has(createModalAccountId.value)) {
    showCreateModal.value = false;
    createModalAccountId.value = null;
    createModalExpectation.value = null;
    createModalCardId.value = null;
  }
  return true;
}

async function reloadAfterControlPlaneConflict(): Promise<void> {
  await reloadControlPlaneView();
}

async function recoverAccountMutationConflict(error: unknown): Promise<boolean> {
  // Only a CAS revision conflict reloads the world. Domain 409s (enable before
  // verify, reorder set mismatch, refresh already running, …) keep the actual
  // backend message and the user's draft instead of a misleading reload.
  if (!isRevisionConflict(error)) return false;
  const session = accountViewSession;
  await reloadAfterControlPlaneConflict();
  // A session change during the reload leaves the dead flow silent.
  if (session !== accountViewSession) return true;
  message.warning(t("账号设置已被其他操作修改，已重新加载最新状态，请重试"));
  return true;
}

/**
 * The Zen card's enabled switch writes through the dedicated provider-settings
 * endpoint with the latest settings revision attached. A 409 means the settings were changed elsewhere:
 * reload accounts/settings and ask the user to retry, in the same style as the
 * settings-page conflict recovery.
 */
async function saveZenProviderSettings(
  account: Account,
  enabled: boolean,
  successMessage?: string,
): Promise<void> {
  if (providerSettingsSaving.value[account.id]) return;
  const session = accountViewSession;
  providerSettingsSaving.value[account.id] = true;
  try {
    const result = await providerApi.updateProviderSettings(account.id, {
      enabled,
    });
    if (session !== accountViewSession) return;
    replaceAccount(result.account);
    if (successMessage) message.success(successMessage);
    void refreshDestinationProjection().then(ok => { if (session === accountViewSession && !ok) notifyDestinationRefreshFailure(); });
  } catch (error) {
    if (session !== accountViewSession) return;
    if (!(await recoverAccountMutationConflict(error))) {
      if (session !== accountViewSession) return;
      message.error(t("保存失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  } finally {
    if (session === accountViewSession) providerSettingsSaving.value[account.id] = false;
  }
}

async function deleteAccount(id: string) {
  const session = accountViewSession;
  try {
    await dashboardApi.deleteAccount(id);
    // A late DELETE receipt after session teardown must not re-mark removals
    // or reschedule persistence in the cleared store.
    if (session !== accountViewSession) return;
    message.success(t("账号已删除"));
    removeAccountState(id);
    const destRefreshed = await refreshDestinationProjection("deleted_refresh_failed");
    if (session !== accountViewSession) return;
    if (!destRefreshed) notifyDestinationRefreshFailure();
    void identitiesStore.loadPresented().catch(() => undefined);
    void providersStore.loadConnections().catch(() => undefined);
  } catch (e) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(e)) return;
    if (session !== accountViewSession) return;
    message.error(t("删除失败：{error}", { error: dashboardErrorDetail(e) }));
  }
}

const quotaRetrying = ref<Record<string, boolean>>({});

async function retryQuotaRecovery(credentialId: string) {
  const credential = destinationsStore.credentials.find((row) => row.id === credentialId);
  if (!quotaRetryRequestNeeded(credential?.quota_recovery) || quotaRetrying.value[credentialId]) return;
  const session = accountViewSession;
  quotaRetrying.value = { ...quotaRetrying.value, [credentialId]: true };
  try {
    await destinationsStore.retryQuotaRecovery(credentialId);
  } catch (e) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(e)) return;
    if (session !== accountViewSession) return;
    message.error(t("重新尝试失败：{error}", { error: dashboardErrorDetail(e) }));
  } finally {
    if (session === accountViewSession) {
      const next = { ...quotaRetrying.value };
      delete next[credentialId];
      quotaRetrying.value = next;
    }
  }
}

async function resetCooldown(id: string) {
  const session = accountViewSession;
  try {
    const updated = await dashboardApi.resetAccountCooldown(id);
    if (session !== accountViewSession) return;
    replaceAccount(updated);
    message.success(t("冷却已重置"));
  } catch (e) {
    if (session !== accountViewSession) return;
    if (await recoverAccountMutationConflict(e)) return;
    if (session !== accountViewSession) return;
    message.error(t("重置失败：{error}", { error: dashboardErrorDetail(e) }));
  }
}

// Full resources belong to explicit editor, setup, or arrangement work. The
// arrival path and its background revalidation use only the bounded page DTO.
const legacyResourcesReady = ref(false);
let legacyBootstrap: Promise<boolean> | null = null;
async function prepareLegacyResources(force = false): Promise<boolean> {
  const pageRevision = accountPage.page?.revision;
  const legacyRevision = destinationsStore.expectation;
  if (!force && legacyResourcesReady.value && pageRevision?.processGeneration === legacyRevision?.processGeneration
    && pageRevision?.revision === legacyRevision?.expectedRevision) return true;
  if (legacyBootstrap) return legacyBootstrap;
  const session = accountViewSession;
  legacyBootstrap = (async () => {
    try {
      await Promise.all([
        accountsStore.loadPresented(), destinationsStore.load(), identitiesStore.loadPresented(),
        providersStore.loadConnections(), loadProviderCatalog(), platformStore.load(),
      ]);
      if (session !== accountViewSession) return false;
      legacyResourcesReady.value = true;
      await nextTick();
      return true;
    } catch (error) {
      if (session === accountViewSession) message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(error) }));
      return false;
    } finally { legacyBootstrap = null; }
  })();
  return legacyBootstrap;
}

const pageActionPending = ref<Record<string, boolean>>({});
const calibrationAccounts = ref<ReadonlySet<string>>(new Set());
function manualReceiptFor(row: PageRow) {
  const id = accountPageRowId(row);
  const slot = billingStore.slotFor(id).value;
  return accountPageQuotaReceipt(row, slot);
}
const expandedPlatformCards = ref<ReadonlySet<string>>(new Set());
function platformSnapshotFor(card: PageCard) { return platformStore.parents.find(parent => parent.id === card.platform?.id)?.snapshot ?? null; }
async function preparePageAccount(row: PageRow): Promise<Account | null> {
  return preparePageAccountId(accountPageRowId(row));
}
async function preparePageAccountId(id: string): Promise<Account | null> {
  const session = accountViewSession;
  const detail = await accountPage.loadDetail(id);
  if (session !== accountViewSession || accountPage.details.get(id) !== detail) return null;
  accountsStore.upsertDetailAccount(detail.account);
  destinationsStore.upsertDetailProjection({
    destinations: detail.destination ? [detail.destination] : [],
    credentials: detail.credential ? [detail.credential] : [],
    expectation: { expectedRevision: detail.revision.revision, processGeneration: detail.revision.processGeneration },
  });
  return detail.account;
}
async function prepareAccountForm(account: Account): Promise<void> {
  if (!providerCatalog.value?.some(entry => entry.provider_id === account.provider_id)) await loadProviderCatalog();
  // The existing credit setup resolves dynamic bindings from these complete
  // inventories. Other edit forms already have their exact endpoint in detail.
  if (findPlanDefinition(account.provider_id, providerCatalog.value)?.dynamic) {
    await Promise.all([identitiesStore.loadPresented(), providersStore.loadConnections()]);
  }
}
async function withPageAccount(row: PageRow, action: (account: Account) => void | Promise<void>): Promise<void> {
  const id = accountPageRowId(row);
  if (pageActionPending.value[id]) return;
  const session = accountViewSession;
  pageActionPending.value = { ...pageActionPending.value, [id]: true };
  try {
    const account = await preparePageAccount(row);
    if (account && session === accountViewSession) await action(account);
  } catch (error) {
    if (session === accountViewSession) message.error(t("操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    if (session === accountViewSession) { const next = { ...pageActionPending.value }; delete next[id]; pageActionPending.value = next; }
  }
}
async function pageRowAction(row: PageRow, key: string): Promise<void> {
  const id = accountPageRowId(row);
  const session = accountViewSession;
  if (key === "refresh-usage") {
    try {
      const receipt = await accountPage.refreshAccount(id, "manual");
      if (session !== accountViewSession || !receipt) return;
      if (receipt.outcome === "partial") {
        const codes = accountPageRefreshErrorCodes(receipt);
        message.warning(codes.length ? codes.map(code => t(platformSnapshotErrorKey(code))).join("；") : t("刷新未完成"));
      } else if (receipt.outcome === "refreshed" && row.platformLink) message.success(t("已刷新"));
      if (receipt.outcome === "refreshed" || receipt.outcome === "partial") await loadAccounts();
    }
    catch (error) { if (session === accountViewSession) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) })); }
    return;
  }
  if (key === "reload-usage") { await loadAccounts(); return; }
  if (key === "calibrate") { await preparePageCalibration(row); return; }
  await withPageAccount(row, async account => {
    if (["fetch-models", "edit-key", "unlink"].includes(key)
      || (key === "models" && platformLinkForAccount(account.id))) {
      if (!await prepareLegacyResources() || session !== accountViewSession) return;
    }
    if (key === "edit") await prepareAccountForm(account);
    else if (key === "refresh-models" && !providerCatalog.value) await loadProviderCatalog();
    if (session !== accountViewSession) return;
    const parent = platformParentForAccount(account.id);
    if (key === "toggle") await toggleAccount(id);
    else if (key === "models") openPlatformKeyModels(id);
    else if (key === "retry-quota") { const credential = destinationsStore.credentialsByLegacyAccountId.get(id); if (credential) await retryQuotaRecovery(credential.id); }
    else {
      if (["move-up", "move-down", "move-to-card"].includes(key)) { await toggleSortModeIfNeeded(); }
      if (["continue-setup", "open-console", "reset-profile"].includes(key)) await loadRegistrationOptions();
      if (session !== accountViewSession) return;
      handleMenuSelect(key, id, parent);
    }
  });
}
async function preparePageCalibration(row: PageRow): Promise<void> {
  const session = accountViewSession;
  await withPageAccount(row, async account => {
    await loadAccountUsage(account.id);
    if (session !== accountViewSession) return;
    focusUsageEditor(account.id);
    calibrationAccounts.value = new Set([...calibrationAccounts.value, account.id]);
  });
}
async function pagePurchaseDate(row: PageRow, date: string): Promise<void> {
  await withPageAccount(row, account => updatePurchaseDate(account.id, date));
}
async function savePageUsage(row: PageRow, key: Parameters<typeof saveUsage>[1]): Promise<void> {
  const session = accountViewSession;
  const id = accountPageRowId(row);
  const before = billingStore.slotFor(id).value;
  await saveUsage(id, key);
  const after = billingStore.slotFor(id).value;
  if (session === accountViewSession && (before?.status !== after?.status || before?.manualReceipt !== after?.manualReceipt)) revalidatePageAfterWrite();
}
function revalidatePageAfterWrite(): void {
  accountPage.noteMutation();
  invalidateManagementPages("accountPage");
  if (accountsViewActive && document.visibilityState === "visible") void loadAccounts();
}
async function toggleSortModeIfNeeded(): Promise<void> { if (!sortMode.value) await toggleSortMode(); }
async function pageRowKeydown(row: PageRow, event: KeyboardEvent): Promise<void> {
  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
  event.preventDefault();
  await withPageAccount(row, async account => { await toggleSortModeIfNeeded(); await moveWithinDisplayedGroup(account.id, event.key === "ArrowUp" ? -1 : 1); });
}
async function pageCardKeydown(cardId: string, event: KeyboardEvent): Promise<void> {
  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
  event.preventDefault();
  await toggleSortModeIfNeeded();
  handleCardKeydown(event, cardId);
}
async function pageCardAction(card: PageCard, key: string): Promise<void> {
  if (key === "edit" && !card.platform) { void router.push(appViewRoute("providers", { destination: card.destination.id, tab: "settings" })); return; }
  const session = accountViewSession;
  if (!await prepareLegacyResources()) return;
  if (session !== accountViewSession) return;
  const group = allGroups.value.find(group => group.id === card.cardId);
  if (!group) return;
  const parent = card.platform ? platformStore.parents.find(parent => parent.id === card.platform!.id) ?? null : null;
  if (key === "platform-models") { const next = new Set(expandedPlatformCards.value); if (next.has(card.cardId)) next.delete(card.cardId); else next.add(card.cardId); expandedPlatformCards.value = next; return; }
  if (key === "add-key") await addKeyForCard(group, parent);
  else if (key === "refresh-parent" && parent) queuePlatformParentRefresh(parent);
  else if (key === "edit" && parent) platformSectionRef.value?.openEdit(parent);
  else if (key === "delete" && parent) platformSectionRef.value?.confirmDelete(parent);
  else if (key === "import-keys" && parent) await platformSectionRef.value?.importKeys(parent);
  else if (key === "link-existing" && parent) platformSectionRef.value?.openLink(parent);
  else if (key === "fetch-all-models" || key === "fetch-models") {
    if (parent) fetchAllPlatformModels(overlayAccountsFor(group));
    else { const account = overlayAccountsFor(group)[0]; if (account) queueAccountModelRefresh(account.id); }
  }
  else if (key === "delete-group" || key === "delete") confirmDeleteEmptyGroup(group.destination);
  else { await toggleSortModeIfNeeded(); if (key === "add-card") await addCardAfter(card.cardId); else if (key === "remove-empty-card") await removeEmptyCardById(card.cardId);
    else if (key.startsWith("move-card-")) await handleCardMove(card.cardId, key.slice("move-card-".length) as RoutingCardMove); }
}
async function loadCardRows(id: string, offset: number): Promise<void> {
  if (accountPage.cardLoading[id]) return;
  const session = accountViewSession;
  const selectedQuery = JSON.stringify(accountPage.query);
  const previous = accountPage.cardPaging[id]?.offset ?? 0;
  try {
    await accountPage.loadCredentials(id, offset);
    if (session !== accountViewSession || selectedQuery !== JSON.stringify(accountPage.query)
      || accountPage.cardPaging[id]?.offset !== offset || previous === offset) return;
    const history = [...(cardPageHistory.value[id] ?? [])];
    if (history.at(-1) === offset) history.pop(); else history.push(previous);
    cardPageHistory.value = { ...cardPageHistory.value, [id]: history };
  } catch { /* Per-card last-good content and error remain visible. */ }
}

let linkPreparation: string | null = null;
async function applyPageAccountDeepLink(): Promise<void> {
  const id = readAccountDeepLink(routeQuerySearch("accounts", route.query));
  if (!id || linkPreparation === id || showModal.value) return;
  linkPreparation = id;
  const session = accountViewSession;
  try {
    const account = await preparePageAccountId(id);
    if (!account || session !== accountViewSession) return;
    await prepareAccountForm(account);
    if (session !== accountViewSession || route.name !== "accounts" || readAccountDeepLink(routeQuerySearch("accounts", route.query)) !== id) return;
    editingAccount.value = account;
    showModal.value = true;
  } catch (error) {
    if (session === accountViewSession) {
      if (error instanceof DashboardRequestError && error.status === 404) { clearAccountDeepLink(); message.warning(t("未找到指定账号，已清除链接参数")); }
      else message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(error) }));
    }
  } finally { if (linkPreparation === id) linkPreparation = null; }
}

const pageOffset = ref(0);
const cardPageHistory = ref<Record<string, number[]>>({});
watch(() => JSON.stringify(accountPage.query), () => { cardPageHistory.value = {}; }, { flush: "sync" });
watch(() => JSON.stringify(accountPage.page?.cards.map(card => card.cardId) ?? []), () => {
  const current = new Set(accountPage.page?.cards.map(card => card.cardId) ?? []);
  cardPageHistory.value = Object.fromEntries(Object.entries(cardPageHistory.value).filter(([id]) => current.has(id)));
}, { flush: "sync" });
function currentPageQuery() { return { search: searchFilter.value, plan: planFilter.value, status: statusFilter.value, offset: pageOffset.value, limit: ACCOUNT_PAGE_SIZE }; }
function routeScalar(value: unknown): string { return typeof value === "string" ? value : ""; }
function readPageFilters(): void {
  searchFilter.value = routeScalar(route.query.search);
  planFilter.value = routeScalar(route.query.plan) || "all";
  const status = routeScalar(route.query.status);
  statusFilter.value = ["available", "cooling", "auth-error", "disabled", "registering"].includes(status) ? status as AccountStatusFilter : "all";
  pageOffset.value = Math.max(0, Number(routeScalar(route.query.offset)) || 0);
}
let filterTimer: number | undefined;
let restoringFilters = false;
function publishPageFilters(): void {
  const query = { ...route.query };
  if (searchFilter.value) query.search = searchFilter.value; else delete query.search;
  if (planFilter.value !== "all") query.plan = planFilter.value; else delete query.plan;
  if (statusFilter.value !== "all") query.status = statusFilter.value; else delete query.status;
  if (pageOffset.value) query.offset = String(pageOffset.value); else delete query.offset;
  void router.replace({ query });
}
watch([searchFilter, planFilter, statusFilter], () => {
  if (restoringFilters || sortMode.value) return;
  if (filterTimer !== undefined) window.clearTimeout(filterTimer);
  filterTimer = window.setTimeout(() => {
    filterTimer = undefined; pageOffset.value = 0; publishPageFilters(); visiblePageRows.value = new Set(); demandedIds.clear();
    if (accountsViewActive) void loadAccounts();
  }, 250);
}, { flush: "sync" });
watch(() => [route.query.search, route.query.plan, route.query.status, route.query.offset], () => {
  if (route.name !== "accounts") return;
  const before = JSON.stringify(currentPageQuery());
  restoringFilters = true; readPageFilters(); restoringFilters = false;
  if (before !== JSON.stringify(currentPageQuery()) && accountsViewActive) { demandedIds.clear(); void loadAccounts(); }
}, { flush: "sync" });
watch(() => route.query.account_id, () => { if (route.name === "accounts") void applyPageAccountDeepLink(); });
function changePage(offset: number): void {
  pageOffset.value = offset; visiblePageRows.value = new Set(); demandedIds.clear(); publishPageFilters(); void loadAccounts();
  document.querySelector(".accounts-view")?.scrollIntoView?.({ block: "start" });
}

const visiblePageRows = ref<ReadonlySet<string>>(new Set());
const demandedIds = new Set<string>();
let demandTimer: number | undefined;
let demandEpoch = 0;
let accountsViewActive = false;
function pageDemandAllowed(): boolean {
  return accountsViewActive && document.visibilityState === "visible" && sessionStore.authenticated && !sortMode.value
    && !busy.value && !platformMutating.value && !showModal.value && !showAddModal.value && !showCredentialModal.value
    && !showCreateModal.value && !showTransfer.value && !showManagedWizard.value;
}
function setPageRowVisible(id: string, visible: boolean): void {
  const next = new Set(visiblePageRows.value); if (visible) next.add(id); else next.delete(id); visiblePageRows.value = next;
  if (visible) scheduleDemand();
}
function scheduleDemand(): void {
  if (demandTimer !== undefined || !pageDemandAllowed()) return;
  demandTimer = window.setTimeout(() => { demandTimer = undefined; void refreshPageDemand(); }, 100);
}
async function refreshPageDemand(): Promise<void> {
  if (!pageDemandAllowed()) return;
  const epoch = demandEpoch;
  const targets = accountPageDemandIds(accountPage.page?.cards ?? [], visiblePageRows.value, collapsedCardIds.value)
    .filter(id => !demandedIds.has(id));
  for (const id of targets) demandedIds.add(id);
  let refreshed = false;
  await mapWithConcurrency(targets, 4, async id => {
    if (epoch !== demandEpoch || !pageDemandAllowed()) return;
    try {
      const receipt = await accountPage.refreshAccount(id, "automatic");
      if (receipt?.outcome === "refreshed" || receipt?.outcome === "partial") refreshed = true;
    } catch { /* Page revalidation retains local facts; automatic errors do not interrupt the user. */ }
  });
  if (refreshed && epoch === demandEpoch && pageDemandAllowed()) await loadAccounts();
}
watch([sortMode, busy, platformMutating, showModal, showAddModal, showCredentialModal, showCreateModal, showTransfer, showManagedWizard], () => { if (pageDemandAllowed()) scheduleDemand(); });

let clock: number | undefined;
let pageTimer: number | undefined;
let activatedOnce = false;
function stopPageTimers(): void {
  demandEpoch++;
  if (clock !== undefined) window.clearTimeout(clock);
  if (pageTimer !== undefined) window.clearTimeout(pageTimer);
  if (demandTimer !== undefined) window.clearTimeout(demandTimer);
  clock = undefined; pageTimer = undefined; demandTimer = undefined;
}
function startPageTimers(): void {
  if (!accountsViewActive || document.visibilityState !== "visible" || !sessionStore.authenticated) return;
  if (clock === undefined) clock = window.setTimeout(() => { clock = undefined; now.value = Date.now(); startPageTimers(); }, 15_000);
  if (pageTimer === undefined) {
    const validUntil = accountPage.page?.validUntil ? Date.parse(accountPage.page.validUntil) : NaN;
    const delay = Number.isFinite(validUntil) ? Math.max(250, Math.min(15_000, validUntil - Date.now())) : 15_000;
    pageTimer = window.setTimeout(async () => {
      pageTimer = undefined;
      if (!accountsViewActive || document.visibilityState !== "visible") return;
      await loadAccounts(); demandedIds.clear(); scheduleDemand(); startPageTimers();
    }, delay);
  }
}
function pageVisibilityChanged(): void {
  if (document.visibilityState !== "visible") { stopPageTimers(); return; }
  if (accountsViewActive && sessionStore.authenticated) { demandedIds.clear(); void loadAccounts().then(() => { scheduleDemand(); startPageTimers(); }); }
}
watch(() => sessionStore.authenticated, ok => {
  if (!ok) { usageReadGeneration++; stopPageTimers(); demandedIds.clear(); visiblePageRows.value = new Set(); refreshQueue.reset();
    legacyResourcesReady.value = false; pageActionPending.value = {}; calibrationAccounts.value = new Set(); cardPageHistory.value = {}; }
}, { flush: "sync" });
watch(() => destinationsStore.expectation, (next, previous) => {
  if (!previous || !next || (next.expectedRevision === previous.expectedRevision && next.processGeneration === previous.processGeneration)) return;
  const detailRead = [...accountPage.details.values()].some(detail => detail.revision.revision === next.expectedRevision
    && detail.revision.processGeneration === next.processGeneration);
  if (detailRead) {
    const rendered = accountPage.page?.revision;
    if (!rendered || rendered.revision !== next.expectedRevision || rendered.processGeneration !== next.processGeneration) void loadAccounts();
    return;
  }
  revalidatePageAfterWrite();
});
watch(() => platformStore.parents, (_next, previous) => { if (previous.length) revalidatePageAfterWrite(); });
watch(() => billingStore.byId, (next, previous) => {
  for (const [id, slot] of Object.entries(next)) {
    if (slot.status && slot.status !== previous[id]?.status) accountPage.commitBilling(id, slot.boundVersion, slot.status);
  }
});

onMounted(() => {
  accountsViewActive = true; restoringFilters = true; readPageFilters(); restoringFilters = false;
  document.addEventListener("visibilitychange", pageVisibilityChanged);
  applyAccountAddDeepLink(); void initializeAccounts().then(startPageTimers);
});
onActivated(() => {
  accountsViewActive = true; now.value = Date.now(); demandedIds.clear();
  applyAccountAddDeepLink();
  if (!activatedOnce) { activatedOnce = true; startPageTimers(); return; }
  void accountPage.load(currentPageQuery(), { maxAgeMs: PAGE_READ_MAX_AGE_MS }).catch(() => undefined)
    .then(() => { void applyPageAccountDeepLink(); scheduleDemand(); startPageTimers(); });
});
onDeactivated(() => { accountsViewActive = false; usageReadGeneration++; stopPageTimers(); sortMode.value = false; cancelArrangement(); });
onUnmounted(() => { accountsViewActive = false; usageReadGeneration++; stopPageTimers(); refreshQueue.reset();
  document.removeEventListener("visibilitychange", pageVisibilityChanged); revertActiveArrangement();
  if (filterTimer !== undefined) window.clearTimeout(filterTimer); });
</script>

<style scoped>
.move-to-card-options { display: grid; gap: var(--ocg-space-md); }
.move-to-card-subject { margin: 0; }
.accounts-view {
  position: relative;
  max-width: 1280px;
  margin: 0 auto;
}

.accounts-content {
  position: relative;
  z-index: 1;
}

.accounts-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: flex-start;
  gap: var(--ocg-space-md) var(--ocg-space-lg);
  min-width: 0;
}

.accounts-actions {
  flex: 0 0 auto;
}

.account-list {
  display: grid;
  gap: var(--ocg-space-md);
}
.account-list-state {
  min-height: 160px;
  display: grid;
  place-items: center;
}

.account-page-pagination {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: var(--ocg-space-sm);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.destination-pending-link { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--ocg-space-sm); }

.accounts-filter-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-md);
  flex: 0 1 auto;
  min-width: 0;
}

.accounts-filter-bar .filter-field {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-xs);
  min-width: 0;
}

.accounts-filter-bar .filter-label {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  line-height: 1.2;
}

.accounts-filter-bar .n-select {
  min-width: 160px;
}

.sort-mode-hint {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  align-self: center;
}

@media (max-width: 640px) {
  .accounts-toolbar {
    gap: var(--ocg-space-md);
  }

  .accounts-filter-bar {
    flex-basis: 100%;
    gap: var(--ocg-space-sm);
  }

  .accounts-actions {
    width: 100%;
    justify-content: flex-end;
  }

  .accounts-filter-bar .filter-field {
    flex: 1 1 calc(50% - 4px);
  }

  .accounts-filter-bar .n-select {
    width: 100%;
  }
}
</style>
