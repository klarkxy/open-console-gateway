<template>
  <div class="cpa-page">
    <n-alert v-if="loadError" type="error" :title="t('加载 CPA 失败：{error}', { error: loadError })">
      <n-button size="small" secondary :loading="loading" @click="load">{{ t("重试") }}</n-button>
    </n-alert>

    <template v-else-if="integration">
      <n-tabs v-model:value="activeTab" type="line" animated class="cpa-tabs" display-directive="if">
        <template #suffix>
          <n-button secondary size="small" :loading="loading" @click="load">
            {{ t("刷新") }}
          </n-button>
        </template>
        <n-tab-pane name="overview" :tab="t('概览')">
      <section class="cpa-section" aria-labelledby="cpa-overview-title">
        <h2 id="cpa-overview-title" class="cpa-section-title sr-only">{{ t("概览") }}</h2>
        <n-space class="cpa-mode-choice" align="center" wrap>
          <span class="cpa-muted">{{ t("选择 CPA 使用方式") }}</span>
          <n-button size="small" :type="mode === 'external' ? 'primary' : 'default'" @click="selectMode('external')">
            {{ t("外部连接") }}
          </n-button>
          <n-button
            size="small"
            :type="mode === 'managed' ? 'primary' : 'default'"
            :disabled="!managedRuntimeAvailable"
            @click="selectMode('managed')"
          >{{ t("托管安装") }}</n-button>
        </n-space>

        <n-alert v-if="runtimeError" type="error" :title="t('加载 CPA 运行时失败：{error}', { error: runtimeError })">
          <n-button size="small" secondary :loading="loading" @click="load">{{ t("重试") }}</n-button>
        </n-alert>

        <template v-if="mode === 'external'">
          <n-alert type="info" :show-icon="false">
            <strong>{{ t("安装 CPA") }}</strong>
            <p>{{ t("连接到你自行启动的本机 CPA；OCG 不会接管、停止或更改该进程。") }}</p>
          </n-alert>

          <n-card size="small" :title="t('基础地址')" class="cpa-card">
            <n-form label-placement="top" @submit.prevent="save">
              <n-form-item :label="t('基础地址')">
                <n-input
                  v-model:value="draft.baseUrl"
                  class="mono"
                  :readonly="integration.baseUrlReadOnly"
                  :disabled="saving"
                  :placeholder="integration.baseUrl"
                  :input-props="{ 'aria-label': t('基础地址') }"
                />
              </n-form-item>
              <n-form-item :label="t('Inference Key')">
                <n-input
                  v-model:value="draft.inferenceKey"
                  type="password"
                  show-password-on="click"
                  :disabled="saving"
                  :placeholder="integration.inferenceKeyConfigured ? t('已设置') : t('未设置')"
                  :input-props="{ 'aria-label': t('Inference Key'), autocomplete: 'new-password' }"
                />
              </n-form-item>
              <n-form-item :label="t('Management Key')">
                <n-input
                  v-model:value="draft.managementKey"
                  type="password"
                  show-password-on="click"
                  :disabled="saving"
                  :placeholder="integration.managementKeyConfigured ? t('已设置') : t('未设置')"
                  :input-props="{ 'aria-label': t('Management Key'), autocomplete: 'new-password' }"
                />
              </n-form-item>
              <p class="cpa-help">{{ t("Key 只会在保存或测试时发送，不会重新显示。") }}</p>
              <n-space wrap>
                <n-button type="primary" attr-type="submit" :loading="saving">
                  {{ t("保存 CPA 配置") }}
                </n-button>
                <n-button :loading="testing" @click="testConnection">{{ t("测试连接") }}</n-button>
                <n-switch
                  :value="integration.enabled"
                  :disabled="!integration.configured || saving"
                  @update:value="setRoutingEnabled"
                >
                  <template #checked>{{ t("路由启用") }}</template>
                  <template #unchecked>{{ t("停用") }}</template>
                </n-switch>
              </n-space>
            </n-form>
          </n-card>

          <n-card size="small" :title="t('版本兼容')" class="cpa-card">
            <div class="cpa-status-grid">
              <StatusCell :label="t('可达性')" :ready="report?.reachable ?? null" :detail="report?.version ?? t('未测试')" />
              <StatusCell :label="t('Management 鉴权')" :ready="report?.managementReady ?? null" :detail="report?.managementError" />
              <StatusCell :label="t('Inference 鉴权')" :ready="report?.inferenceReady ?? null" :detail="report?.inferenceError" />
            </div>
          </n-card>

          <n-card v-if="integration.configured" size="small" class="cpa-card cpa-danger" :title="t('断开并清除')">
            <p>{{ t("确定断开 CPA 并清除 OCG 保存的地址、两把 Key、路由账号和模型目录吗？CPA 自己的 OAuth 数据不会被删除。") }}</p>
            <n-button type="error" secondary :loading="disconnecting" @click="confirmDisconnect">{{ t("断开并清除") }}</n-button>
          </n-card>
        </template>

        <template v-else>
          <n-alert v-if="mode === 'unsupported'" type="warning" :title="t('当前环境不支持托管 CPA 运行时')">
            {{ integration.runtimeUnavailableReason || runtime?.unavailableReason || t("当前平台无官方 CLIProxyAPI 构建（支持 Windows x64、macOS、Linux x64），改用外部连接。") }}
          </n-alert>

          <template v-else>
            <n-card size="small" :title="t('托管运行时')" class="cpa-card">
              <div class="cpa-status-grid">
                <div class="cpa-status-cell">
                  <span class="cpa-muted">{{ t("运行状态") }}</span>
                  <n-tag :type="integration.runtimeRunning ? 'success' : 'default'" size="small">{{ runtimeStateDetail }}</n-tag>
                </div>
                <div class="cpa-status-cell">
                  <span class="cpa-muted">{{ t("当前版本") }}</span>
                  <span>{{ integration.installedVersion ?? runtime?.currentVersion ?? "—" }}</span>
                </div>
                <div class="cpa-status-cell">
                  <span class="cpa-muted">{{ t("最新版本") }}</span>
                  <span>{{ runtimeCheck?.latestVersion ?? integration.latestVersion ?? runtime?.latestVersion ?? t("未检查") }}</span>
                  <span v-if="runtimeCheck?.updateAvailable ?? integration.updateAvailable" class="cpa-status-detail">
                    {{ t("发现新版本 {version}", { version: runtimeCheck?.latestVersion ?? integration.latestVersion ?? t("未检查") }) }}
                  </span>
                  <span v-else-if="runtimeCheck" class="cpa-status-detail">{{ t("已是最新版本") }}</span>
                </div>
                <div class="cpa-status-cell">
                  <span class="cpa-muted">{{ t("上一版本") }}</span>
                  <span>{{ runtime?.previousVersion ?? "—" }}</span>
                </div>
              </div>

              <div v-if="runtime && (runtime.phase !== 'idle' || runtimeOperationDetail)" class="cpa-phase">
                <n-tag :type="runtime.phase === 'failed' ? 'error' : 'warning'" size="small">
                  {{ runtimePhaseLabel(runtime.phase) }}
                </n-tag>
                <span v-if="runtimeOperationDetail" class="cpa-status-detail">{{ runtimeOperationDetail }}</span>
                <span v-if="runtime.phase === 'failed' && runtime.error" class="cpa-status-detail">{{ runtime.error }}</span>
              </div>

              <n-alert v-if="runtimePollError" type="error" :title="t('CPA 运行时状态刷新失败：{error}', { error: runtimePollError })">
                <n-button size="small" secondary @click="retryRuntimePoll">{{ t("重试") }}</n-button>
              </n-alert>

              <n-space v-if="runtime" wrap class="cpa-runtime-actions">
                <n-button
                  type="primary"
                  size="small"
                  :disabled="!controls.install"
                  :loading="runtimeAction === 'install'"
                  @click="installRuntime"
                >{{ t("安装") }}</n-button>
                <n-button
                  size="small"
                  :disabled="!controls.start"
                  :loading="runtimeAction === 'start'"
                  @click="startRuntime"
                >{{ t("启动") }}</n-button>
                <n-button
                  size="small"
                  :disabled="!controls.stop"
                  :loading="runtimeAction === 'stop'"
                  @click="stopRuntime"
                >{{ t("停止") }}</n-button>
                <n-button
                  size="small"
                  :disabled="!controls.checkUpdate"
                  :loading="runtimeAction === 'checkUpdate'"
                  @click="checkUpdate"
                >{{ t("检查更新") }}</n-button>
                <n-button
                  size="small"
                  :disabled="!controls.update"
                  :loading="runtimeAction === 'update'"
                  @click="updateRuntime"
                >{{ updateLabel }}</n-button>
                <n-button
                  size="small"
                  :disabled="!controls.rollback"
                  :loading="runtimeAction === 'rollback'"
                  @click="rollbackRuntime"
                >{{ rollbackLabel }}</n-button>
                <n-button
                  size="small"
                  type="error"
                  secondary
                  :disabled="!controls.remove"
                  :loading="runtimeAction === 'remove'"
                  @click="confirmRemoveRuntime"
                >{{ t("移除") }}</n-button>
              </n-space>
            </n-card>
          </template>
        </template>

        <n-card v-if="showClientKeys" size="small" :title="t('客户端 Key')" class="cpa-card">
          <template #header-extra>
            <n-space wrap>
              <n-button
                size="small"
                type="primary"
                :disabled="!!keyAction"
                :loading="keyAction === 'create'"
                @click="addClientKey"
              >{{ t("添加客户端 Key") }}</n-button>
            </n-space>
          </template>
          <n-alert v-if="revealedSecret" type="success" class="cpa-secret" :title="t('新 Key 仅显示这一次')">
            <p>{{ t("立即复制并妥善保存；关闭后无法再次查看。") }}</p>
            <div class="cpa-secret-row">
              <code class="mono cpa-secret-value">{{ revealedSecret.secret }}</code>
              <n-space wrap>
                <n-button size="small" @click="copyRevealedSecret">
                  {{ copiedTarget === "cpa-runtime-secret" ? t("Key 已复制") : t("复制 Key") }}
                </n-button>
                <n-button size="small" secondary @click="dismissRevealedSecret">{{ t("我已保存，关闭") }}</n-button>
              </n-space>
            </div>
          </n-alert>

          <div v-if="keysLoading" class="cpa-state"><n-spin size="small" /></div>
          <n-alert v-else-if="keysError" type="error" :title="t('加载客户端 Key 失败：{error}', { error: keysError })">
            <n-button size="small" secondary @click="loadRuntimeKeys">{{ t("重试") }}</n-button>
          </n-alert>
          <template v-else>
            <div v-if="runtimeKeys.length" class="cpa-key-list">
              <CpaKeyRow
                v-for="key in keyPartition.protectedKeys"
                :key="key.fingerprint"
                :runtime-key="key"
                :key-action="keyAction"
                @rotate="rotateClientKey"
                @delete="confirmDeleteClientKey"
              />
              <CpaKeyRow
                v-for="key in keyPartition.directKeys"
                :key="key.fingerprint"
                :runtime-key="key"
                :key-action="keyAction"
                @rotate="rotateClientKey"
                @delete="confirmDeleteClientKey"
              />
            </div>
            <n-empty v-else :description="t('暂无客户端 Key')" />
          </template>
        </n-card>
      </section>
        </n-tab-pane>

        <n-tab-pane name="accounts" :tab="t('账号')">
      <section class="cpa-section" aria-labelledby="cpa-accounts-title">
        <h2 id="cpa-accounts-title" class="cpa-section-title sr-only">{{ t("账号") }}</h2>

        <n-card size="small" :title="t('CPA OAuth 账号')" class="cpa-card">
          <div class="cpa-cli-import-head"><strong>{{ t("新登录") }}</strong></div>
          <n-space wrap class="oauth-providers">
            <template v-for="provider in CPA_OAUTH_PROVIDERS" :key="provider.id">
              <template v-if="provider.id === 'codex'">
                <n-button
                  secondary
                  :disabled="!integration.configured || !!oauth || !!oauthStartingAction || !!cliImporting"
                  :loading="oauthStartingAction === 'codex:browser'"
                  @click="startOAuth('codex', 'browser')"
                >{{ t("Codex 浏览器登录") }}</n-button>
                <n-button
                  secondary
                  :disabled="!integration.configured || !codexDeviceLoginAvailable || !!oauth || !!oauthStartingAction || !!cliImporting"
                  :loading="oauthStartingAction === 'codex:device'"
                  @click="startOAuth('codex', 'device')"
                >{{ t("Codex 设备码登录") }}</n-button>
              </template>
              <n-button
                v-else
                secondary
                :disabled="!integration.configured || !!oauth || !!oauthStartingAction || !!cliImporting"
                :loading="oauthStartingAction === `${provider.id}:browser`"
                @click="startOAuth(provider.id, 'browser')"
              >{{ t("登录 {provider}", { provider: provider.label }) }}</n-button>
            </template>
          </n-space>
          <p v-if="!codexDeviceLoginAvailable" class="cpa-help">
            {{ t("设备码登录需要托管 CPA 运行；外部连接使用浏览器登录。") }}
          </p>
          <n-alert v-if="codexBrowserFailure" type="warning" class="cpa-oauth-status" :title="t('Codex 浏览器登录失败')">
            <p>{{ codexBrowserFailure }}</p>
            <template v-if="codexDeviceLoginAvailable">
              <p>{{ t("如果本机授权回调服务不可用，可改用设备码登录完成授权。") }}</p>
              <n-button
                size="small"
                :disabled="!!oauth || !!oauthStartingAction || !!cliImporting"
                :loading="oauthStartingAction === 'codex:device'"
                @click="startOAuth('codex', 'device')"
              >{{ t("改用设备码登录") }}</n-button>
            </template>
            <p v-else>{{ t("设备码登录需要托管 CPA 处于运行状态。") }}</p>
          </n-alert>
          <n-alert v-if="oauth" type="info" class="cpa-oauth-status" :show-icon="false">
            <template v-if="oauth.flow === 'device'">
              <p v-if="oauth.provider === 'codex'">{{ t("打开授权页面并输入下方设备码；确认 ChatGPT 账号的安全设置或工作区允许设备登录。") }}</p>
              <p v-else>{{ t("打开授权页面并输入下方设备码完成授权。") }}</p>
              <div v-if="oauth.userCode" class="cpa-device-code-row">
                <code class="mono cpa-device-code">{{ oauth.userCode }}</code>
                <n-button size="small" @click="copyDeviceCode">
                  {{ copiedTarget === "cpa-device-code" ? t("已复制设备码") : t("复制设备码") }}
                </n-button>
              </div>
              <n-space align="center" wrap>
                <n-button v-if="oauth.url" size="small" type="primary" tag="a" :href="oauth.url" target="_blank" rel="noopener noreferrer">
                  {{ t("打开授权页面") }}
                </n-button>
                <span v-if="deviceCodeExpiryMinutes" class="cpa-muted">
                  {{ t("设备码约 {minutes} 分钟后过期", { minutes: deviceCodeExpiryMinutes }) }}
                </span>
                <n-button size="small" secondary :loading="oauthCancelling" @click="cancelOAuth">{{ t("取消当前授权") }}</n-button>
              </n-space>
            </template>
            <template v-else>
              <p>{{ t("正在等待 CPA 完成授权…") }}</p>
              <n-space align="center" wrap>
                <n-button v-if="oauth.url" size="small" type="primary" tag="a" :href="oauth.url" target="_blank" rel="noopener noreferrer">
                  {{ t("打开授权页面") }}
                </n-button>
                <n-tag v-if="oauth.userCode" type="warning">{{ t("设备码：{code}", { code: oauth.userCode }) }}</n-tag>
                <n-button size="small" secondary :loading="oauthCancelling" @click="cancelOAuth">{{ t("取消当前授权") }}</n-button>
              </n-space>
            </template>
          </n-alert>

          <div class="cpa-cli-import">
            <div class="cpa-cli-import-head">
              <strong>{{ t("导入本机 CLI 已登录账号") }}</strong>
              <n-button size="small" quaternary :loading="cliImportsLoading" @click="loadCliImports">{{ t("重新检测") }}</n-button>
            </div>
            <p class="cpa-help">
              {{ t("导入时复制到 CPA，源文件不会被修改。") }}
            </p>
            <n-alert v-if="cliImportsError" type="warning" :title="t('检测本机 CLI 账号失败：{error}', { error: cliImportsError })">
              <n-button size="small" secondary :loading="cliImportsLoading" @click="loadCliImports">{{ t("重试") }}</n-button>
            </n-alert>
            <div v-else-if="cliImportsLoading && cliImports.length === 0" class="cpa-state"><n-spin size="small" /></div>
            <template v-else-if="cliImports.length">
              <n-space wrap class="oauth-providers">
                <n-button
                  v-for="source in cliImports"
                  :key="source.provider"
                  secondary
                  :disabled="cliImportDisabled(source)"
                  :loading="cliImporting === source.provider"
                  @click="importCliAccount(source)"
                >{{ t("导入 {provider}", { provider: cliImportProviderLabel(source.provider) }) }}</n-button>
              </n-space>
              <p v-if="cliImportBlockedSources.length" class="cpa-help cpa-cli-import-tip">
                <n-tooltip trigger="hover">
                  <template #trigger>
                    <span class="cpa-cli-import-tip-text">{{ cliImportBlockedTip }}</span>
                  </template>
                  <p
                    v-for="source in cliImportBlockedSources"
                    :key="`${source.provider}-reason`"
                  >{{ cliImportBlockedText(source) }}</p>
                </n-tooltip>
              </p>
            </template>
            <p v-else class="cpa-help">{{ t("未检测到可导入的本机 CLI 账号。") }}</p>
            <n-alert v-if="cliImportNotice" :type="cliImportNotice.type" class="cpa-oauth-status" :show-icon="false">
              <p>{{ cliImportNotice.text }}</p>
              <n-button
                v-if="cliImportNotice.type === 'warning'"
                size="small"
                secondary
                :loading="accountsLoading"
                @click="refreshAccountsAfterImport"
              >{{ t("刷新账号列表") }}</n-button>
            </n-alert>
          </div>

          <div v-if="accountsLoading" class="cpa-state"><n-spin size="small" /></div>
          <n-alert v-else-if="accountsError" type="error" :title="t('CPA 账号操作失败：{error}', { error: accountsError })">
            <n-button size="small" secondary @click="loadAccounts">{{ t("重试") }}</n-button>
          </n-alert>
          <n-empty v-else-if="cpaAccounts.length === 0" :description="t('暂无账号')" />
          <div v-else class="cpa-account-list">
            <article v-for="account in cpaAccounts" :key="cpaAccountKey(account)" class="cpa-account-row">
              <div class="cpa-account-main">
                <div class="cpa-account-title">
                  <strong>{{ account.label || account.name }}</strong>
                  <n-tag v-if="account.runtimeOnly" type="warning" size="small">{{ t("运行时插件账号，仅供查看") }}</n-tag>
                  <n-tag v-else :type="account.disabled || account.unavailable ? 'default' : 'success'" size="small">
                    {{ account.status || (account.disabled ? t("已禁用") : account.unavailable ? t("不可用") : t("可用")) }}
                  </n-tag>
                </div>
                <span class="cpa-muted">{{ account.provider }}<template v-if="account.email"> · {{ account.email }}</template></span>
                <span v-if="account.statusMessage" class="cpa-muted">{{ account.statusMessage }}</span>
                <span v-if="formatCpaQuota(account.quota)" class="cpa-muted">{{ t("配额") }} · {{ formatCpaQuota(account.quota) }}</span>
              </div>
              <n-space v-if="account.mutable && !account.runtimeOnly && account.authIndex" wrap>
                <n-button size="small" :loading="accountAction === cpaAccountKey(account)" @click="setAccountStatus(account, !account.disabled)">
                  {{ account.disabled ? t("启用") : t("停用") }}
                </n-button>
                <n-tooltip v-if="account.authIndex" trigger="hover">
                  <template #trigger>
                    <n-button size="small" :loading="accountAction === cpaAccountKey(account)" @click="resetQuota(account)">
                      {{ t("重置配额") }}
                    </n-button>
                  </template>
                  {{ t("清除 CPA 为本账号记录的本机用量，不会重置供应商官方额度。") }}
                </n-tooltip>
                <n-button size="small" type="error" secondary :loading="accountAction === cpaAccountKey(account)" @click="confirmDeleteAccount(account)">
                  {{ t("删除") }}
                </n-button>
              </n-space>
            </article>
          </div>
        </n-card>
      </section>
        </n-tab-pane>

        <n-tab-pane name="catalog" :tab="t('模型目录')">
      <section class="cpa-section" aria-labelledby="cpa-catalog-title">
        <h2 id="cpa-catalog-title" class="cpa-section-title sr-only">{{ t("模型目录") }}</h2>
        <n-card size="small" class="cpa-card">
          <div class="cpa-catalog-head">
            <div class="cpa-catalog-meta">
              <span>{{ modelCatalogDetail }}</span>
              <span v-if="catalogModels.length">{{ t("已选 {selected} / {total}", { selected: catalogSelectedCount, total: catalogModels.length }) }}</span>
              <span v-if="catalogSourceUrl" class="mono">{{ t("来源") }} · {{ catalogSourceUrl }}</span>
            </div>
            <n-space wrap>
              <n-button
                size="small"
                :disabled="!catalogModels.length || catalogAllEnabled"
                @click="setCatalogEnabledAll(true)"
              >{{ t("全部开启") }}</n-button>
              <n-button
                size="small"
                :disabled="!catalogModels.length || catalogNoneEnabled"
                @click="setCatalogEnabledAll(false)"
              >{{ t("全部关闭") }}</n-button>
              <n-button
                type="primary"
                :disabled="!integration.configured"
                :loading="refreshingModels"
                @click="refreshModels"
              >{{ catalogRefreshingLabel }}</n-button>
            </n-space>
          </div>
          <p v-if="catalogModels.length" class="cpa-help">{{ t("点选加入路由；新发现的默认关闭。") }}</p>
          <n-alert v-if="!integration.configured" type="info" :title="t('先在概览中配置并启动 CPA，再刷新模型目录。')">
            <n-button size="small" @click="activeTab = 'overview'">{{ t('返回概览') }}</n-button>
          </n-alert>
          <div v-else-if="catalogLoading" class="cpa-state"><n-spin size="small" /></div>
          <n-alert v-else-if="catalogError" type="error" :title="t('加载模型目录失败：{error}', { error: catalogError })">
            <n-button size="small" secondary @click="loadCatalog">{{ t("重试") }}</n-button>
          </n-alert>
          <p v-else-if="catalogModels.length === 0" class="cpa-help">{{ t("尚未刷新模型目录；启用路由前需先刷新。") }}</p>
          <div v-else class="cpa-catalog-groups">
            <section v-for="group in catalogGroups" :key="group.source || 'unknown'" class="cpa-catalog-group">
              <h3>{{ group.source || t("未知来源") }} · {{ group.models.length }}</h3>
              <div class="cpa-catalog-cards">
                <button
                  v-for="model in group.models"
                  :key="model.id"
                  type="button"
                  class="cpa-catalog-card"
                  :class="{ 'is-selected': model.enabled }"
                  :aria-pressed="model.enabled"
                  :aria-label="model.id"
                  @click="toggleCatalogModel(model)"
                >
                  <code class="mono">{{ model.id }}</code>
                </button>
              </div>
            </section>
          </div>
        </n-card>
      </section>
        </n-tab-pane>

        <n-tab-pane v-if="mode === 'managed'" name="logs" :tab="t('运行时日志')">
      <section class="cpa-section" aria-labelledby="cpa-logs-title">
        <h2 id="cpa-logs-title" class="cpa-section-title sr-only">{{ t("运行时日志") }}</h2>

        <n-card size="small" class="cpa-card">
          <template v-if="runtime?.installed" #header-extra>
            <n-button size="small" quaternary :loading="logsLoading" @click="refreshLogs">{{ t("刷新日志") }}</n-button>
          </template>
          <n-alert v-if="!runtime?.installed" type="info" :title="t('CPA 尚未安装，请先在概览中安装。')">
            <n-button size="small" @click="activeTab = 'overview'">{{ t('返回概览') }}</n-button>
          </n-alert>
          <div v-else-if="logsLoading" class="cpa-state"><n-spin size="small" /></div>
          <n-alert v-else-if="logsError" type="error" :title="t('加载 CPA 运行时日志失败：{error}', { error: logsError })">
            <n-button size="small" secondary @click="refreshLogs">{{ t("重试") }}</n-button>
          </n-alert>
          <template v-else-if="logs">
            <template v-if="stdoutTail || stderrTail">
              <h3 class="cpa-log-title">{{ t("标准输出") }}</h3>
              <pre class="cpa-log mono">{{ stdoutTail || t("暂无日志") }}</pre>
              <h3 class="cpa-log-title">{{ t("标准错误") }}</h3>
              <pre class="cpa-log mono">{{ stderrTail || t("暂无日志") }}</pre>
            </template>
            <n-empty v-else :description="t('暂无日志')" />
          </template>
          <n-empty v-else :description="t('暂无日志')" />
        </n-card>
      </section>
        </n-tab-pane>
      </n-tabs>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, h, onActivated, onBeforeUnmount, onDeactivated, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NEmpty,
  NForm,
  NFormItem,
  NInput,
  NSpin,
  NSpace,
  NSwitch,
  NTabPane,
  NTabs,
  NTag,
  NTooltip,
  useDialog,
  useMessage,
} from "naive-ui";
import type {
  CpaAccount,
  CpaCliImports,
  CpaConnectionReport,
  CpaIntegration,
  CpaOAuthProvider,
  CpaOAuthStart,
  CpaRuntime,
  CpaRuntimeCheck,
  CpaRuntimeKey,
  CpaRuntimeLogs,
  CpaRuntimePhase,
  MutationExpectation,
} from "../api/generated/dashboard-v3.ts";
import { dashboardV3 } from "../api/dashboard-v3.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type { CpaCatalog, CpaCatalogEntry } from "../api/generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useClipboard } from "../utils/format.ts";
import {
  CPA_OAUTH_PROVIDERS,
  cpaAccountKey,
  cpaCliImportAlreadyPresent,
  cpaOAuthProviderForCliAccount,
  cpaClientKeysAvailable,
  cpaLogTail,
  cpaManagedRuntimeConfirmed,
  cpaRuntimeControls,
  cpaRuntimeMode,
  formatCpaQuota,
  groupCpaCatalogModels,
  partitionCpaRuntimeKeys,
  isCpaOAuthSuccessStatus,
  isCpaOAuthTerminalStatus,
  isCpaPhaseBusy,
} from "../domain/cpa-runtime.ts";
import type { CpaRuntimeModePreference } from "../domain/cpa-runtime.ts";
import CpaKeyRow from "../components/CpaKeyRow.vue";

const dialog = useDialog();
const message = useMessage();
const controlPlane = useControlPlaneStore();
const { copiedTarget, copy, cleanup: cleanupClipboard } = useClipboard();

const integration = ref<CpaIntegration | null>(null);
const report = ref<CpaConnectionReport | null>(null);
const cpaAccounts = ref<CpaAccount[]>([]);
const loading = ref(false);
const loadError = ref("");
const saving = ref(false);
const testing = ref(false);
const refreshingModels = ref(false);
const catalogModels = ref<CpaCatalogEntry[]>([]);
const catalogSourceUrl = ref<string | null>(null);
const catalogLoading = ref(false);
const catalogError = ref("");
const accountsLoading = ref(false);
const accountsError = ref("");
const accountAction = ref("");
const disconnecting = ref(false);
const oauth = ref<CpaOAuthStart | null>(null);
// Single-flight key `${provider}:${method}` for the start request in flight.
const oauthStartingAction = ref<string | null>(null);
const oauthCancelling = ref(false);
// Last Codex browser-flow failure, kept so the page can offer the device
// alternative without ever switching flows on its own.
const codexBrowserFailure = ref<string | null>(null);
let oauthTimer: number | null = null;
let oauthPollGeneration = 0;

type CpaOAuthMethod = "browser" | "device";

// CLI import discovery is metadata-only: provider, source label, and support
// flags. No credential text or arbitrary path ever enters the browser.
type CpaCliImportSource = CpaCliImports["sources"][number];
const cliImports = ref<CpaCliImportSource[]>([]);
const cliImportsLoading = ref(false);
const cliImportsError = ref("");
// Single-flight provider key, mutually exclusive with any OAuth flow.
const cliImporting = ref<string | null>(null);
const cliImportNotice = ref<{ type: "success" | "warning"; text: string } | null>(null);
// Providers confirmed imported this session. Complements account-name detection
// so a successful import greys out immediately even if the account list is empty.
const importedCliProviders = ref<CpaOAuthProvider[]>([]);

const runtime = ref<CpaRuntime | null>(null);
const runtimeError = ref("");
const runtimePollError = ref("");
const runtimeCheck = ref<CpaRuntimeCheck | null>(null);
const runtimeAction = ref("");
let runtimeTimer: number | null = null;
let runtimePollGeneration = 0;

const logs = ref<CpaRuntimeLogs | null>(null);
const logsLoading = ref(false);
const logsError = ref("");

const activeTab = ref("overview");

const runtimeKeys = ref<CpaRuntimeKey[]>([]);
const keysLoading = ref(false);
const keysError = ref("");
const keyAction = ref("");
// One-time reveal area: the only component state that ever holds a client-key
// secret. Dismiss or any page refresh clears it; list rows stay secret-free.
const revealedSecret = ref<{ fingerprint: string; hint: string; secret: string } | null>(null);

const draft = ref({ baseUrl: "", inferenceKey: "", managementKey: "" });
const modePreference = ref<CpaRuntimeModePreference>(null);

const mode = computed(() => (
  integration.value ? cpaRuntimeMode(integration.value, runtime.value, modePreference.value) : "external"
));
const managedRuntimeAvailable = computed(() => (
  integration.value ? cpaManagedRuntimeConfirmed(integration.value, runtime.value) : false
));
const showClientKeys = computed(() => cpaClientKeysAvailable(runtime.value));
// Device sign-in rides on the managed runtime's local auth flow; an external
// CPA stays browser-only.
const codexDeviceLoginAvailable = computed(() => (
  mode.value === "managed"
  && integration.value?.runtimeOwned === true
  && integration.value.runtimeRunning === true
));
const deviceCodeExpiryMinutes = computed(() => {
  const seconds = oauth.value?.flow === "device" ? oauth.value.expiresIn : null;
  return seconds ? Math.max(1, Math.ceil(seconds / 60)) : null;
});
const controls = computed(() => cpaRuntimeControls({
  runtime: runtime.value,
  busy: runtimeAction.value !== "",
  updateCheck: runtimeCheck.value,
}));
const keyPartition = computed(() => partitionCpaRuntimeKeys(runtimeKeys.value));
const catalogGroups = computed(() => groupCpaCatalogModels(catalogModels.value));
const catalogSelectedCount = computed(() => catalogModels.value.filter((model) => model.enabled).length);
const catalogAllEnabled = computed(() => (
  catalogModels.value.length > 0 && catalogSelectedCount.value === catalogModels.value.length
));
const catalogNoneEnabled = computed(() => catalogSelectedCount.value === 0);
const catalogRefreshingLabel = computed(() => (
  refreshingModels.value ? t("正在刷新模型目录…") : t("刷新模型目录")
));
// Catalog writes stay serial, but every click captures its own target snapshot
// at enqueue time: a slow PUT response must never overwrite a newer selection.
// Only the latest write of the current generation may apply a response or run
// an error resync. Load, model refresh, disconnect, and page exit bump the
// generation so queued writes from before them return quietly instead of
// resurrecting a cleared integration or overwriting freshly loaded data.
let catalogWriteGeneration = 0;
let catalogWriteSeq = 0;
let catalogWriteChain = Promise.resolve();

function bumpCatalogWriteGeneration(): void {
  catalogWriteGeneration += 1;
}

type CatalogWriteEpoch = { generation: number; seq: number };

function currentCatalogWrite(): CatalogWriteEpoch {
  return { generation: catalogWriteGeneration, seq: catalogWriteSeq };
}

function isCurrentCatalogWrite(epoch: CatalogWriteEpoch): boolean {
  return epoch.generation === catalogWriteGeneration && epoch.seq === catalogWriteSeq;
}
const stdoutTail = computed(() => cpaLogTail(logs.value?.stdout ?? ""));
const stderrTail = computed(() => cpaLogTail(logs.value?.stderr ?? ""));

const runtimeStateDetail = computed(() => {
  if (!runtime.value || !runtime.value.installed) return t("未安装");
  return integration.value?.runtimeRunning ? t("运行中") : t("已停止");
});

const runtimeOperationDetail = computed(() => (
  integration.value?.currentOperation ?? runtime.value?.currentOperation ?? ""
));

const updateLabel = computed(() => (
  (runtimeCheck.value?.updateAvailable ?? integration.value?.updateAvailable)
    ? t("更新到 {version}", { version: runtimeCheck.value?.latestVersion ?? integration.value?.latestVersion ?? t("未检查") })
    : t("更新")
));

const rollbackLabel = computed(() => (
  runtime.value?.previousVersion
    ? t("回滚到 {version}", { version: runtime.value.previousVersion })
    : t("回滚")
));

const modelCatalogDetail = computed(() => {
  const count = catalogModels.value.length || integration.value?.modelCount || 0;
  if (count === 0) return t("未测试");
  const refreshed = integration.value?.modelsRefreshedAt
    ? new Date(integration.value.modelsRefreshedAt).toLocaleString()
    : "";
  return refreshed ? `${count} · ${refreshed}` : String(count);
});

function runtimePhaseLabel(phase: CpaRuntimePhase): string {
  switch (phase) {
    case "checking": return t("检查中");
    case "downloading": return t("下载中");
    case "installing": return t("安装中");
    case "starting": return t("启动中");
    case "failed": return t("失败");
    default: return t("空闲");
  }
}

function selectMode(next: Exclude<CpaRuntimeModePreference, null>): void {
  if (next === "managed" && !managedRuntimeAvailable.value) return;
  modePreference.value = next;
}

async function runMutation<T>(run: (expectation: MutationExpectation) => Promise<T>): Promise<T> {
  if (!controlPlane.hasTokens()) await controlPlane.refresh();
  return controlPlane.runMutation(run);
}

async function load(): Promise<void> {
  bumpRuntimePollGeneration();
  bumpCliImportGeneration();
  bumpCatalogWriteGeneration();
  const generation = runtimePollGeneration;
  loading.value = true;
  loadError.value = "";
  runtimePollError.value = "";
  // A full refresh never keeps a previously revealed secret around.
  revealedSecret.value = null;
  try {
    const value = await dashboardV3.getCpaIntegration();
    if (generation !== runtimePollGeneration) return;
    integration.value = value;
    draft.value.baseUrl = value.baseUrl;
    // Secrets are write-only. Refreshing state must never repopulate either input.
    draft.value.inferenceKey = "";
    draft.value.managementKey = "";
    runtimeError.value = "";
    try {
      const snapshot = await dashboardV3.getCpaRuntime();
      if (generation !== runtimePollGeneration) return;
      runtime.value = snapshot;
      applyRuntimeSnapshot(snapshot);
    } catch (error) {
      if (generation !== runtimePollGeneration) return;
      runtime.value = null;
      runtimeError.value = dashboardErrorDetail(error);
    }
    if (value.configured) {
      await loadAccounts();
      await loadCatalog();
      // Discovery failures surface inline and never break the page or OAuth.
      await loadCliImports();
    } else {
      cpaAccounts.value = [];
      resetCatalog();
      resetCliImports();
    }
    if (generation !== runtimePollGeneration) return;
    if (cpaClientKeysAvailable(runtime.value)) await loadRuntimeKeys();
    else runtimeKeys.value = [];
    if (generation !== runtimePollGeneration) return;
    syncRuntimePolling();
  } catch (error) {
    if (generation !== runtimePollGeneration) return;
    loadError.value = dashboardErrorDetail(error);
  } finally {
    loading.value = false;
  }
}

async function save(): Promise<void> {
  if (saving.value || !integration.value) return;
  saving.value = true;
  try {
    const updated = await runMutation((expectation) => dashboardV3.putCpaIntegration({
      ...(integration.value!.baseUrlReadOnly ? {} : { baseUrl: draft.value.baseUrl.trim() || null }),
      ...(draft.value.inferenceKey.trim() ? { inferenceKey: draft.value.inferenceKey.trim() } : {}),
      ...(draft.value.managementKey.trim() ? { managementKey: draft.value.managementKey.trim() } : {}),
      enabled: integration.value!.enabled,
    }, expectation));
    integration.value = updated;
    draft.value.baseUrl = updated.baseUrl;
    draft.value.inferenceKey = "";
    draft.value.managementKey = "";
    message.success(t("CPA 配置已保存"));
    await loadAccounts();
    await loadCatalog();
  } catch (error) {
    message.error(t("CPA 配置失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    saving.value = false;
  }
}

async function testConnection(): Promise<void> {
  if (testing.value || !integration.value) return;
  testing.value = true;
  try {
    report.value = await dashboardV3.testCpaIntegration({
      ...(integration.value.baseUrlReadOnly ? {} : { baseUrl: draft.value.baseUrl.trim() || null }),
      ...(draft.value.inferenceKey.trim() ? { inferenceKey: draft.value.inferenceKey.trim() } : {}),
      ...(draft.value.managementKey.trim() ? { managementKey: draft.value.managementKey.trim() } : {}),
    });
  } catch (error) {
    message.error(t("CPA 连接测试失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    testing.value = false;
  }
}

async function setRoutingEnabled(enabled: boolean): Promise<void> {
  if (!integration.value || saving.value) return;
  saving.value = true;
  try {
    integration.value = await runMutation((expectation) => dashboardV3.putCpaIntegration({ enabled }, expectation));
  } catch (error) {
    message.error(t("CPA 配置失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    saving.value = false;
  }
}

function catalogEnabledIds(models: readonly CpaCatalogEntry[]): string[] {
  return models.filter((model) => model.enabled).map((model) => model.id);
}

function applyCatalog(snapshot: Pick<CpaCatalog, "models" | "sourceUrl" | "refreshedAt">): void {
  catalogModels.value = snapshot.models;
  catalogSourceUrl.value = snapshot.sourceUrl;
  catalogError.value = "";
  if (integration.value) {
    integration.value = {
      ...integration.value,
      modelCount: snapshot.models.length,
      modelsRefreshedAt: snapshot.refreshedAt,
    };
  }
}

function resetCatalog(): void {
  catalogModels.value = [];
  catalogSourceUrl.value = null;
  catalogError.value = "";
}

async function fetchCatalog(epoch?: CatalogWriteEpoch): Promise<void> {
  const snapshot = await dashboardV4.getCpaCatalog();
  // A read that started before a newer selection intent, or before a newer
  // loaded generation, must not overwrite either one when it resolves.
  if (epoch && !isCurrentCatalogWrite(epoch)) return;
  applyCatalog(snapshot);
}

async function loadCatalog(): Promise<void> {
  if (catalogLoading.value) return;
  catalogLoading.value = true;
  catalogError.value = "";
  const epoch = currentCatalogWrite();
  try {
    await fetchCatalog(epoch);
  } catch (error) {
    catalogError.value = dashboardErrorDetail(error);
  } finally {
    catalogLoading.value = false;
  }
}

function persistCatalogSelection(): void {
  const generation = catalogWriteGeneration;
  const seq = (catalogWriteSeq += 1);
  const enabledIds = catalogEnabledIds(catalogModels.value);
  const isCurrentWrite = () => generation === catalogWriteGeneration && seq === catalogWriteSeq;
  catalogWriteChain = catalogWriteChain.then(async () => {
    if (!isCurrentWrite()) return;
    try {
      const snapshot = await runMutation((expectation) => dashboardV4.putCpaCatalog({ enabledIds }, expectation));
      // A newer click or a newer loaded generation owns the UI now.
      if (!isCurrentWrite()) return;
      applyCatalog(snapshot);
    } catch (error) {
      if (!isCurrentWrite()) return;
      const detail = dashboardErrorDetail(error);
      try {
        await fetchCatalog({ generation, seq });
        // A newer click owns the selection now; its own save reports the outcome.
        if (!isCurrentWrite()) return;
      } catch (reloadError) {
        if (!isCurrentWrite()) return;
        catalogError.value = dashboardErrorDetail(reloadError);
      }
      message.error(t("CPA 模型选择失败：{error}", { error: detail }));
    }
  });
}

function toggleCatalogModel(model: CpaCatalogEntry): void {
  catalogModels.value = catalogModels.value.map((row) => (
    row.id === model.id ? { ...row, enabled: !row.enabled } : row
  ));
  persistCatalogSelection();
}

function setCatalogEnabledAll(enabled: boolean): void {
  catalogModels.value = catalogModels.value.map((row) => ({ ...row, enabled }));
  persistCatalogSelection();
}

async function refreshModels(): Promise<void> {
  if (refreshingModels.value) return;
  bumpCatalogWriteGeneration();
  const epoch = currentCatalogWrite();
  refreshingModels.value = true;
  try {
    await runMutation((expectation) => dashboardV3.refreshCpaModels(expectation));
    const snapshot = await dashboardV4.getCpaCatalog();
    // A selection made while the refresh was in flight owns the UI now;
    // its own save applies instead of this older snapshot.
    if (isCurrentCatalogWrite(epoch)) applyCatalog(snapshot);
    message.success(t("模型目录已刷新，共 {count} 个模型", { count: snapshot.models.length }));
  } catch (error) {
    message.error(t("CPA 模型刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    refreshingModels.value = false;
  }
}

async function loadAccounts(): Promise<void> {
  accountsLoading.value = true;
  accountsError.value = "";
  try {
    cpaAccounts.value = (await dashboardV3.getCpaAccounts()).accounts;
  } catch (error) {
    accountsError.value = dashboardErrorDetail(error);
  } finally {
    accountsLoading.value = false;
  }
}

async function setAccountStatus(account: CpaAccount, disabled: boolean): Promise<void> {
  accountAction.value = cpaAccountKey(account);
  try {
    await runMutation((expectation) => dashboardV3.setCpaAccountStatus({
      name: account.name,
      authIndex: account.authIndex!,
      disabled,
    }, expectation));
    await loadAccounts();
  } catch (error) {
    message.error(t("CPA 账号操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    accountAction.value = "";
  }
}

async function resetQuota(account: CpaAccount): Promise<void> {
  if (!account.authIndex) return;
  accountAction.value = cpaAccountKey(account);
  try {
    await runMutation((expectation) => dashboardV3.resetCpaQuota({
      name: account.name,
      authIndex: account.authIndex!,
    }, expectation));
    await loadAccounts();
  } catch (error) {
    message.error(t("CPA 账号操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    accountAction.value = "";
  }
}

function confirmDeleteAccount(account: CpaAccount): void {
  dialog.warning({
    title: t("删除 CPA 账号"),
    content: t("确定删除 CPA 中的账号 {name} 吗？这会删除 CPA 保存的 OAuth 凭据。", { name: account.label || account.name }),
    positiveText: t("删除"),
    negativeText: t("取消"),
    onPositiveClick: () => deleteAccount(account),
  });
}

async function deleteAccount(account: CpaAccount): Promise<void> {
  accountAction.value = cpaAccountKey(account);
  try {
    await runMutation((expectation) => dashboardV3.deleteCpaAccount({
      name: account.name,
      authIndex: account.authIndex!,
    }, expectation));
    await loadAccounts();
    const importedProvider = cpaOAuthProviderForCliAccount(account);
    if (importedProvider && !cpaCliImportAlreadyPresent(importedProvider, cpaAccounts.value)) {
      unmarkCliProviderImported(importedProvider);
    }
  } catch (error) {
    message.error(t("CPA 账号操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    accountAction.value = "";
  }
}

async function startOAuth(provider: CpaOAuthProvider, method: CpaOAuthMethod): Promise<void> {
  if (oauth.value || oauthStartingAction.value) return;
  if (method === "device" && !codexDeviceLoginAvailable.value) return;
  // A new flow invalidates any poll still in flight from a previous one.
  bumpOAuthPollGeneration();
  const generation = oauthPollGeneration;
  oauthStartingAction.value = `${provider}:${method}`;
  codexBrowserFailure.value = null;
  try {
    const started = await runMutation((expectation) => dashboardV3.startCpaOAuth({ provider, method }, expectation));
    if (generation !== oauthPollGeneration) {
      // The page left or the flow was superseded while the start was in flight:
      // never adopt the session, but release it server-side on a best-effort basis.
      void runMutation((expectation) => dashboardV3.cancelCpaOAuth({ state: started.state }, expectation)).catch(() => {});
      return;
    }
    oauth.value = started;
    // Device sign-in needs the short code read first; only browser flows auto-open.
    if (method === "browser" && started.url) window.open(started.url, "_blank", "noopener,noreferrer");
    scheduleOAuthPoll();
  } catch (error) {
    if (generation !== oauthPollGeneration) return;
    const detail = dashboardErrorDetail(error);
    // A Codex browser start that fails before any state exists (for example the
    // local callback server never came up) gets the same device alternative as
    // a mid-flow failure.
    if (provider === "codex" && method === "browser") codexBrowserFailure.value = detail;
    message.error(t("CPA 账号操作失败：{error}", { error: detail }));
  } finally {
    oauthStartingAction.value = null;
  }
}

function bumpOAuthPollGeneration(): void {
  oauthPollGeneration += 1;
  stopOAuthPoll();
}

// Completion-scheduled single flight: the next poll is queued only after the
// previous response has been applied, so a slow status read never overlaps itself.
function scheduleOAuthPoll(): void {
  stopOAuthPoll();
  oauthTimer = window.setTimeout(() => void pollOAuth(), 3000);
}

function stopOAuthPoll(): void {
  if (oauthTimer !== null) window.clearTimeout(oauthTimer);
  oauthTimer = null;
}

async function pollOAuth(): Promise<void> {
  const active = oauth.value;
  if (!active) return;
  const generation = oauthPollGeneration;
  const flowState = active.state;
  try {
    const status = await dashboardV3.getCpaOAuthStatus(flowState);
    // Cancel, leaving the page, or a newer flow invalidates this response.
    if (generation !== oauthPollGeneration || oauth.value?.state !== flowState) return;
    if (isCpaOAuthTerminalStatus(status.status)) {
      bumpOAuthPollGeneration();
      const finished = oauth.value;
      oauth.value = null;
      if (isCpaOAuthSuccessStatus(status.status)) await loadAccounts();
      else {
        if (status.error) message.warning(status.error);
        if (finished?.provider === "codex" && finished.flow !== "device") {
          codexBrowserFailure.value = status.error ?? t("授权未完成");
        }
      }
    } else {
      scheduleOAuthPoll();
    }
  } catch (error) {
    if (generation !== oauthPollGeneration || oauth.value?.state !== flowState) return;
    bumpOAuthPollGeneration();
    const failed = oauth.value;
    oauth.value = null;
    const detail = dashboardErrorDetail(error);
    if (failed?.provider === "codex" && failed.flow !== "device") codexBrowserFailure.value = detail;
    message.error(t("CPA 账号操作失败：{error}", { error: detail }));
  }
}

async function cancelOAuth(): Promise<void> {
  const active = oauth.value;
  // Invalidate synchronously: an in-flight poll must not mutate state or
  // re-arm the timer while the cancel request is still on the wire.
  bumpOAuthPollGeneration();
  if (!active) return;
  const generation = oauthPollGeneration;
  oauthCancelling.value = true;
  try {
    await runMutation((expectation) => dashboardV3.cancelCpaOAuth({ state: active.state }, expectation));
  } catch {
    // A page close must not surface a second error over the original OAuth result.
  } finally {
    // A slow cancel must not clear a flow started after this one was invalidated.
    if (generation === oauthPollGeneration && oauth.value?.state === active.state) oauth.value = null;
    oauthCancelling.value = false;
  }
}

async function copyDeviceCode(): Promise<void> {
  const code = oauth.value?.flow === "device" ? oauth.value.userCode : null;
  if (!code) return;
  try {
    await copy("cpa-device-code", code, t("设备码"));
    message.success(t("已复制设备码"));
  } catch (error) {
    message.error(error instanceof Error ? error.message : t("复制失败"));
  }
}

function cancelOAuthOnLeave(): void {
  // Bump even with no visible flow so a pending start resolves into a no-op.
  bumpOAuthPollGeneration();
  if (!oauth.value) return;
  void cancelOAuth();
}

// --- CLI import ---

function cliImportProviderLabel(provider: CpaOAuthProvider): string {
  return CPA_OAUTH_PROVIDERS.find((entry) => entry.id === provider)?.label ?? provider;
}

function cliImportAlreadyDone(provider: CpaOAuthProvider): boolean {
  return importedCliProviders.value.includes(provider)
    || cpaCliImportAlreadyPresent(provider, cpaAccounts.value);
}

function cliImportDisabled(source: CpaCliImportSource): boolean {
  return !integration.value?.configured
    || !source.supported
    || !source.available
    || cliImportAlreadyDone(source.provider)
    || !!oauth.value
    || !!oauthStartingAction.value
    || (!!cliImporting.value && cliImporting.value !== source.provider);
}

const cliImportBlockedSources = computed(() => (
  cliImports.value.filter((source) => !source.supported || !source.available)
));

const cliImportBlockedTip = computed(() => {
  const providers = cliImportBlockedSources.value
    .map((source) => cliImportProviderLabel(source.provider))
    .join("、");
  return t("{providers} 无法从本机导入，改用上方的新登录。", { providers });
});

function cliImportBlockedText(source: CpaCliImportSource): string {
  const reason = source.reason
    ?? (!source.supported ? t("暂不支持导入该来源") : t("未检测到本机登录信息"));
  return `${cliImportProviderLabel(source.provider)} · ${source.source} · ${reason}`;
}

function markCliProviderImported(provider: CpaOAuthProvider): void {
  if (!importedCliProviders.value.includes(provider)) {
    importedCliProviders.value = [...importedCliProviders.value, provider];
  }
}

function unmarkCliProviderImported(provider: CpaOAuthProvider): void {
  importedCliProviders.value = importedCliProviders.value.filter((id) => id !== provider);
}

function resetCliImports(): void {
  cliImports.value = [];
  cliImportsError.value = "";
  cliImportNotice.value = null;
  importedCliProviders.value = [];
}

// Discovery and import responses older than the latest load, disconnect, or
// page exit are ignored: they must not mutate UI state, and an import that may
// already be committed server-side is never "undone" from here.
let cliImportGeneration = 0;

function bumpCliImportGeneration(): void {
  cliImportGeneration += 1;
  // Discovery is read-only and idempotent; letting a superseding load re-issue
  // it is fine, the generation check keeps only the newest response.
  cliImportsLoading.value = false;
}

async function loadCliImports(): Promise<void> {
  if (cliImportsLoading.value) return;
  const generation = cliImportGeneration;
  cliImportsLoading.value = true;
  cliImportsError.value = "";
  try {
    const result = await dashboardV3.getCpaCliImports();
    if (generation !== cliImportGeneration) return;
    cliImports.value = result.sources;
  } catch (error) {
    if (generation !== cliImportGeneration) return;
    cliImportsError.value = dashboardErrorDetail(error);
  } finally {
    cliImportsLoading.value = false;
  }
}

// Import runs only on an explicit click and only one flow of any kind at a time.
async function importCliAccount(source: CpaCliImportSource): Promise<void> {
  if (cliImporting.value || oauth.value || oauthStartingAction.value) return;
  if (!integration.value?.configured || !source.supported || !source.available) return;
  if (cliImportAlreadyDone(source.provider)) return;
  const generation = cliImportGeneration;
  cliImporting.value = source.provider;
  cliImportNotice.value = null;
  try {
    const result = await runMutation((expectation) => dashboardV3.importCpaCliAccount({ provider: source.provider }, expectation));
    if (generation !== cliImportGeneration) return;
    if (result.outcome === "unconfirmed") {
      cliImportNotice.value = {
        type: "warning",
        text: t("导入结果未确认：先刷新账号列表确认是否已导入，再决定是否重试；重复导入按生成的文件名幂等处理，不会产生重复账号。"),
      };
    } else {
      // Product copy names the provider, never the hashed implementation file.
      const provider = cliImportProviderLabel(result.provider);
      markCliProviderImported(result.provider);
      cliImportNotice.value = {
        type: "success",
        text: result.outcome === "alreadyImported"
          ? t("{provider} 账号已存在，无需重复导入。", { provider })
          : t("已导入 {provider} 账号。", { provider }),
      };
      await loadAccounts();
    }
  } catch (error) {
    if (generation !== cliImportGeneration) return;
    message.error(t("CPA 账号操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    cliImporting.value = null;
  }
}

// The unconfirmed warning offers this as its only follow-up: a plain account
// list refresh. It never re-runs or retries the import.
async function refreshAccountsAfterImport(): Promise<void> {
  if (accountsLoading.value) return;
  await loadAccounts();
}

function confirmDisconnect(): void {
  dialog.warning({
    title: t("断开并清除"),
    content: t("确定断开 CPA 并清除 OCG 保存的地址、两把 Key、路由账号和模型目录吗？CPA 自己的 OAuth 数据不会被删除。"),
    positiveText: t("断开并清除"),
    negativeText: t("取消"),
    onPositiveClick: () => disconnect(),
  });
}

async function disconnect(): Promise<void> {
  bumpCliImportGeneration();
  bumpCatalogWriteGeneration();
  disconnecting.value = true;
  try {
    await runMutation((expectation) => dashboardV3.deleteCpaIntegration(expectation));
    integration.value = await dashboardV3.getCpaIntegration();
    cpaAccounts.value = [];
    resetCatalog();
    resetCliImports();
    report.value = null;
    draft.value = { baseUrl: integration.value.baseUrl, inferenceKey: "", managementKey: "" };
    message.success(t("CPA 已断开"));
  } catch (error) {
    message.error(t("CPA 配置失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    disconnecting.value = false;
  }
}

// --- managed runtime lifecycle ---

function bumpRuntimePollGeneration(): void {
  runtimePollGeneration += 1;
  stopRuntimePoll();
}

function syncRuntimePolling(): void {
  stopRuntimePoll();
  if (runtime.value && isCpaPhaseBusy(runtime.value.phase)) {
    runtimeTimer = window.setTimeout(() => void pollRuntime(), 2000);
  }
}

function stopRuntimePoll(): void {
  if (runtimeTimer !== null) window.clearTimeout(runtimeTimer);
  runtimeTimer = null;
}

async function pollRuntime(): Promise<void> {
  const generation = runtimePollGeneration;
  runtimePollError.value = "";
  try {
    const next = await dashboardV3.getCpaRuntime();
    if (generation !== runtimePollGeneration) return;
    runtime.value = next;
    applyRuntimeSnapshot(next);
    if (!isCpaPhaseBusy(next.phase)) {
      stopRuntimePoll();
      await refreshAfterRuntimeSettled();
    } else {
      syncRuntimePolling();
    }
  } catch (error) {
    if (generation !== runtimePollGeneration) return;
    stopRuntimePoll();
    runtimePollError.value = dashboardErrorDetail(error);
  }
}

function retryRuntimePoll(): Promise<void> {
  return pollRuntime();
}

/** Keep the Overview truthful while the full integration refresh is in flight. */
function applyRuntimeSnapshot(snapshot: CpaRuntime): void {
  if (!integration.value) return;
  integration.value = {
    ...integration.value,
    currentOperation: snapshot.currentOperation,
    installedVersion: snapshot.currentVersion,
    latestVersion: snapshot.latestVersion,
    runtimeOwned: snapshot.owned,
    runtimeRunning: snapshot.running,
    runtimeSupported: snapshot.supported,
    runtimeUnavailableReason: snapshot.unavailableReason,
    updateAvailable: snapshot.updateAvailable,
  };
}

/** A settled lifecycle operation can change routing config, accounts, models, and key eligibility. */
async function refreshAfterRuntimeSettled(): Promise<void> {
  const generation = runtimePollGeneration;
  try {
    const next = await dashboardV3.getCpaIntegration();
    if (generation !== runtimePollGeneration) return;
    integration.value = next;
  } catch {
    // The runtime snapshot remains visible and the header retry can recover the integration read.
    return;
  }
  if (integration.value.configured) {
    await loadAccounts();
    await loadCatalog();
    await loadCliImports();
  } else {
    cpaAccounts.value = [];
    resetCatalog();
    resetCliImports();
  }
  if (generation !== runtimePollGeneration) return;
  if (cpaClientKeysAvailable(runtime.value)) await loadRuntimeKeys();
  else runtimeKeys.value = [];
}

async function runRuntimeAction(
  name: string,
  run: (expectation: MutationExpectation) => Promise<CpaRuntime>,
): Promise<void> {
  if (runtimeAction.value) return;
  bumpRuntimePollGeneration();
  const generation = runtimePollGeneration;
  runtimeAction.value = name;
  runtimePollError.value = "";
  // A lifecycle change invalidates the previous update check.
  runtimeCheck.value = null;
  try {
    const next = await runMutation(run);
    if (generation !== runtimePollGeneration) return;
    runtime.value = next;
    applyRuntimeSnapshot(next);
    if (isCpaPhaseBusy(next.phase)) syncRuntimePolling();
    else {
      stopRuntimePoll();
      await refreshAfterRuntimeSettled();
    }
  } catch (error) {
    if (generation !== runtimePollGeneration) return;
    message.error(t("CPA 运行时操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    runtimeAction.value = "";
  }
}

function installRuntime(): Promise<void> {
  return runRuntimeAction("install", (expectation) => dashboardV3.installCpaRuntime({}, expectation));
}

function startRuntime(): Promise<void> {
  return runRuntimeAction("start", (expectation) => dashboardV3.startCpaRuntime(expectation));
}

function stopRuntime(): Promise<void> {
  return runRuntimeAction("stop", (expectation) => dashboardV3.stopCpaRuntime(expectation));
}

async function checkUpdate(): Promise<void> {
  if (runtimeAction.value) return;
  bumpRuntimePollGeneration();
  const generation = runtimePollGeneration;
  runtimeAction.value = "checkUpdate";
  try {
    runtimeCheck.value = await runMutation((expectation) => dashboardV3.checkCpaRuntimeUpdate(expectation));
    if (generation !== runtimePollGeneration) return;
    runtime.value = await dashboardV3.getCpaRuntime();
    if (generation !== runtimePollGeneration) return;
    applyRuntimeSnapshot(runtime.value);
    syncRuntimePolling();
  } catch (error) {
    if (generation !== runtimePollGeneration) return;
    message.error(t("CPA 运行时操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    runtimeAction.value = "";
  }
}

function updateRuntime(): Promise<void> {
  const check = runtimeCheck.value;
  if (!check?.updateAvailable) return Promise.resolve();
  return runRuntimeAction("update", (expectation) => dashboardV3.updateCpaRuntime({
    expectedVersion: check.latestVersion,
  }, expectation));
}

function rollbackRuntime(): Promise<void> {
  return runRuntimeAction("rollback", (expectation) => dashboardV3.rollbackCpaRuntime(expectation));
}

function confirmRemoveRuntime(): void {
  dialog.warning({
    title: t("移除 CPA 运行时"),
    content: t("确定移除 OCG 管理的 CPA 运行时吗？本机安装文件、CPA OAuth 凭据和本地运行时配置将被删除。"),
    positiveText: t("移除"),
    negativeText: t("取消"),
    onPositiveClick: () => runRuntimeAction("remove", (expectation) => dashboardV3.removeCpaRuntime(expectation)),
  });
}

// --- runtime logs ---

// Logs live in their own tab: load them on first visit, and fall back to the
// overview tab if the active pane disappears with the managed runtime.
watch([activeTab, mode, () => runtime.value?.installed], () => {
  if (activeTab.value === "keys") activeTab.value = "overview";
  if (activeTab.value === "logs" && mode.value !== "managed") activeTab.value = "overview";
  if (activeTab.value === "logs" && !logs.value && !logsLoading.value) void refreshLogs();
});

async function refreshLogs(): Promise<void> {
  if (logsLoading.value || !runtime.value?.installed) return;
  logsLoading.value = true;
  logsError.value = "";
  try {
    logs.value = await dashboardV3.getCpaRuntimeLogs();
  } catch (error) {
    logsError.value = dashboardErrorDetail(error);
  } finally {
    logsLoading.value = false;
  }
}

// --- client keys ---

async function loadRuntimeKeys(): Promise<void> {
  keysLoading.value = true;
  keysError.value = "";
  try {
    runtimeKeys.value = (await dashboardV3.getCpaRuntimeKeys()).keys;
  } catch (error) {
    keysError.value = dashboardErrorDetail(error);
  } finally {
    keysLoading.value = false;
  }
}

async function addClientKey(): Promise<void> {
  if (keyAction.value) return;
  keyAction.value = "create";
  try {
    const created = await runMutation((expectation) => dashboardV3.createCpaRuntimeKey(expectation));
    revealedSecret.value = { fingerprint: created.fingerprint, hint: created.hint, secret: created.secret };
    await loadRuntimeKeys();
  } catch (error) {
    message.error(t("客户端 Key 操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    keyAction.value = "";
  }
}

async function rotateClientKey(key: CpaRuntimeKey): Promise<void> {
  if (keyAction.value) return;
  keyAction.value = `rotate:${key.fingerprint}`;
  try {
    const rotated = await runMutation((expectation) => dashboardV3.rotateCpaRuntimeKey(key.fingerprint, expectation));
    revealedSecret.value = { fingerprint: rotated.fingerprint, hint: rotated.hint, secret: rotated.secret };
    await loadRuntimeKeys();
  } catch (error) {
    message.error(t("客户端 Key 操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    keyAction.value = "";
  }
}

function confirmDeleteClientKey(key: CpaRuntimeKey): void {
  if (key.protected) return;
  dialog.warning({
    title: t("删除客户端 Key"),
    content: t("确定删除客户端 Key {hint} 吗？使用该 Key 的客户端将立即失效。", { hint: key.hint }),
    positiveText: t("删除"),
    negativeText: t("取消"),
    onPositiveClick: () => deleteClientKey(key),
  });
}

async function deleteClientKey(key: CpaRuntimeKey): Promise<void> {
  keyAction.value = `delete:${key.fingerprint}`;
  try {
    await runMutation((expectation) => dashboardV3.deleteCpaRuntimeKey(key.fingerprint, expectation));
    if (revealedSecret.value?.fingerprint === key.fingerprint) revealedSecret.value = null;
    await loadRuntimeKeys();
  } catch (error) {
    message.error(t("客户端 Key 操作失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    keyAction.value = "";
  }
}

async function copyRevealedSecret(): Promise<void> {
  if (!revealedSecret.value) return;
  try {
    await copy("cpa-runtime-secret", revealedSecret.value.secret, "Key");
    message.success(t("Key 已复制"));
  } catch (error) {
    message.error(error instanceof Error ? error.message : t("复制失败"));
  }
}

function dismissRevealedSecret(): void {
  revealedSecret.value = null;
}

const StatusCell = (props: { label: string; ready: boolean | null; detail?: string | null }) => h("div", { class: "cpa-status-cell" }, [
  h("span", { class: "cpa-muted" }, props.label),
  h(NTag, { type: props.ready === true ? "success" : props.ready === false ? "error" : "default", size: "small" }, {
    default: () => props.ready === true ? t("已就绪") : props.ready === false ? t("未就绪") : t("未测试"),
  }),
  props.detail ? h("span", { class: "cpa-status-detail" }, props.detail) : null,
]);

onMounted(() => {
  window.addEventListener("pagehide", cancelOAuthOnLeave);
  void load();
});
onActivated(() => { if (!loading.value) void load(); });
onDeactivated(() => {
  cancelOAuthOnLeave();
  bumpRuntimePollGeneration();
  bumpCliImportGeneration();
  bumpCatalogWriteGeneration();
});
onBeforeUnmount(() => {
  window.removeEventListener("pagehide", cancelOAuthOnLeave);
  cancelOAuthOnLeave();
  bumpRuntimePollGeneration();
  bumpCliImportGeneration();
  bumpCatalogWriteGeneration();
  cleanupClipboard();
});
</script>

<style scoped>
.cpa-page { display: grid; gap: var(--ocg-space-lg); max-width: 1060px; }
.cpa-help, .cpa-danger p { margin: 6px 0 0; color: var(--ocg-muted); line-height: 1.6; }
.cpa-section { display: grid; gap: var(--ocg-space-md); }
.cpa-section-title { margin: var(--ocg-space-sm) 0 0; color: var(--ocg-ink); font-size: var(--ocg-font-lg); }
.cpa-tabs :deep(.n-tabs-nav) { margin-bottom: var(--ocg-space-md); }
.cpa-card { box-shadow: var(--ocg-shadow-sm); }
.cpa-status-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: var(--ocg-space-md); }
.cpa-status-cell { display: grid; gap: 6px; min-width: 0; align-content: start; }
.cpa-status-detail, .cpa-muted { overflow-wrap: anywhere; color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
.cpa-phase { display: flex; flex-wrap: wrap; align-items: center; gap: var(--ocg-space-sm); margin-top: 14px; }
.cpa-runtime-actions { margin-top: 14px; }
.cpa-log-title { margin: 0; color: var(--ocg-muted); font-size: var(--ocg-font-sm); font-weight: 600; }
.cpa-log {
  box-sizing: border-box;
  width: 100%;
  max-height: 240px;
  margin: 0;
  padding: 10px var(--ocg-space-md);
  overflow: auto;
  border: 1px solid var(--ocg-divider);
  border-radius: 8px;
  background: var(--ocg-surface);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-xs);
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.cpa-oauth-status { margin-top: 14px; }
.cpa-oauth-status p { margin-top: 0; }
.cpa-device-code-row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--ocg-space-md); margin: 10px 0 14px; }
.cpa-device-code {
  padding: var(--ocg-space-sm) 14px;
  border: 1px solid var(--ocg-divider);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-surface);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-lg);
  letter-spacing: 0.08em;
  overflow-wrap: anywhere;
}
.cpa-cli-import { display: grid; gap: var(--ocg-space-sm); margin-top: var(--ocg-space-lg); }
.cpa-cli-import-head { display: flex; align-items: center; justify-content: space-between; gap: var(--ocg-space-md); margin-top: 2px; color: var(--ocg-ink); }
.cpa-cli-import .cpa-help { margin: 0; }
.cpa-cli-import-tip { width: fit-content; max-width: 100%; }
.cpa-cli-import-tip-text {
  border-bottom: 1px dotted currentColor;
  cursor: help;
}
.cpa-state { display: grid; justify-content: center; padding: 20px; }
.cpa-catalog-head { display: flex; align-items: center; justify-content: space-between; gap: var(--ocg-space-lg); }
.cpa-catalog-meta { display: flex; flex-wrap: wrap; gap: var(--ocg-space-sm) var(--ocg-space-lg); color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
.cpa-catalog-groups { display: grid; gap: var(--ocg-space-lg); margin-top: 14px; }
.cpa-catalog-group h3 { margin: 0 0 var(--ocg-space-sm); color: var(--ocg-muted); font-size: var(--ocg-font-sm); font-weight: 600; }
.cpa-catalog-cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: var(--ocg-space-sm); }
.cpa-catalog-card {
  margin: 0;
  padding: var(--ocg-space-sm) 10px;
  border: 1px solid var(--ocg-divider);
  border-radius: var(--ocg-radius-md);
  background: var(--ocg-surface);
  color: var(--ocg-ink);
  text-align: left;
  cursor: pointer;
  overflow-wrap: anywhere;
}
.cpa-catalog-card.is-selected {
  border-color: var(--ocg-success);
  background: var(--ocg-success-soft);
}
.cpa-account-list, .cpa-key-list { display: grid; gap: var(--ocg-space-sm); margin-top: 14px; }
.cpa-account-row { display: flex; align-items: center; justify-content: space-between; gap: var(--ocg-space-lg); padding: var(--ocg-space-md); border: 1px solid var(--ocg-divider); border-radius: var(--ocg-radius-md); }
.cpa-account-main { display: grid; gap: var(--ocg-space-xs); min-width: 0; }
.cpa-account-title { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; color: var(--ocg-ink); }
.cpa-secret { margin-bottom: 14px; }
.cpa-secret p { margin: 0 0 10px; }
.cpa-secret-row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--ocg-space-md); }
.cpa-secret-value {
  padding: 6px 10px;
  border: 1px solid var(--ocg-divider);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-surface);
  color: var(--ocg-ink);
  overflow-wrap: anywhere;
}
.cpa-danger { border-color: color-mix(in srgb, var(--ocg-error) 34%, var(--ocg-divider)); }
@media (max-width: 760px) {
  .cpa-account-row, .cpa-catalog-head { align-items: stretch; flex-direction: column; }
  .cpa-status-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
</style>
