<!--
  组合式 (self-contained) 堆叠柱状图。纯 SVG + Vue computed,零第三方依赖。
  设计参考 Vercel / Linear / Stripe 现代克制的 dashboard 风格:
    - 细网格线、克制的坐标轴
    - 圆角柱条、垂直渐变填充
    - hover 显示当日各模型明细 tooltip
    - 图例使用小圆点 + 模型名,颜色按模型稳定分配
  颜色使用共享色板；第一系列跟随当前主题强调色，其余系列保持固定。
-->
<template>
  <div ref="rootRef" class="stacked-bar-chart">
    <svg
      :viewBox="`0 0 ${width} ${height}`"
      :width="width"
      :height="height"
      preserveAspectRatio="xMidYMid meet"
      class="chart-svg"
      role="group"
      :aria-labelledby="`chart-title-${gid}`"
      :aria-describedby="`chart-description-${gid}`"
    >
      <title :id="`chart-title-${gid}`">{{ t("最近 {days} 天按模型分段的每日 Token 消耗", { days }) }}</title>
      <desc :id="`chart-description-${gid}`">{{ chartDescription }}</desc>
      <defs>
        <linearGradient
          v-for="(c, idx) in CHART_PALETTE"
          :id="`bar-grad-${idx}-${gid}`"
          :key="idx"
          x1="0"
          y1="0"
          x2="0"
          y2="1"
        >
          <stop offset="0%" :stop-color="c" stop-opacity="0.95" />
          <stop offset="100%" :stop-color="c" stop-opacity="0.85" />
        </linearGradient>
      </defs>

      <!-- 横向网格线 + Y 轴刻度 -->
      <g class="grid">
        <line
          v-for="t in yTicks"
          :key="`g-${t.value}`"
          :x1="padL"
          :x2="width - padR"
          :y1="t.y"
          :y2="t.y"
          class="grid-line"
        />
        <text
          v-for="t in yTicks"
          :key="`y-${t.value}`"
          :x="padL - 8"
          :y="t.y + 3"
          text-anchor="end"
          class="axis-text"
        >{{ t.label }}</text>
      </g>

      <!-- 柱条 -->
      <g class="bars">
        <g
          v-for="(bar, bi) in bars"
          :key="`col-${bi}`"
          class="bar-col"
          :transform="`translate(${bar.x}, 0)`"
          :tabindex="dates[bi]?.total > 0 ? 0 : -1"
          role="img"
          :aria-label="barAriaLabels[bi]"
          @pointerenter="onEnter(bi, $event)"
          @pointermove="onMove(bi, $event)"
          @pointerleave="onLeave"
          @focus="onFocus(bi)"
          @blur="onLeave"
          @keydown.esc="onLeave"
        >
          <rect
            v-for="(seg, si) in bar.segments"
            :key="si"
            :x="2"
            :y="seg.y"
            :width="barWidth - 4"
            :height="seg.h"
            :fill="`url(#bar-grad-${seg.idx}-${gid})`"
            :rx="si === 0 ? 3 : 0"
            :ry="si === bar.segments.length - 1 ? 3 : 0"
            class="bar-seg"
          />
          <!-- 透明 hit-box 让整列都可 hover,即使柱条之间有间隙 -->
          <rect
            :x="0"
            :y="padT"
            :width="barWidth"
            :height="chartH"
            fill="transparent"
            class="bar-hitbox"
          />
        </g>
      </g>

      <!-- X 轴日期 -->
      <g class="x-axis">
        <text
          v-for="(label, i) in xLabels"
          :key="`x-${i}`"
          :x="label.x"
          :y="height - padB + 16"
          text-anchor="middle"
          class="axis-text"
        >{{ label.text }}</text>
      </g>
    </svg>

    <!-- Teleport 避开 dashboard card 的 overflow 裁剪；fixed 坐标按 viewport 计算。 -->
    <Teleport to="body">
      <div
        v-if="tooltipVisible"
        ref="tooltipRef"
        class="chart-tooltip"
        role="tooltip"
      >
        <div class="tooltip-title">{{ tooltipContent.title }}</div>
        <div class="tooltip-total">{{ t("合计 {total}", { total: formatTokens(tooltipContent.total) }) }}</div>
        <div
          v-for="row in tooltipContent.rows"
          :key="row.model"
          class="tooltip-row"
        >
          <span class="dot" :style="{ background: row.color }" />
          <span class="model">{{ row.model }}</span>
          <span class="tokens">{{ formatTokens(row.tokens) }}</span>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, nextTick, onMounted, onBeforeUnmount, useId } from "vue";
import type { DailyModelTokens } from "../api/dashboard";
import { CHART_PALETTE } from "../theme";
import { locale, t } from "../i18n/index.ts";
import { formatTokens } from "../utils/format.ts";

const props = withDefaults(defineProps<{
  data: DailyModelTokens[];
  days?: number; // 实际展示的天数(用于补零)
}>(), {
  days: 30,
});

// --- 布局常量 ---
const padL = 104;
const padR = 16;
const padT = 16;
const padB = 28;
const width = ref(720);
const height = 280;
const gid = useId(); // 渐变 id 唯一化,避免多实例冲突

const rootRef = ref<HTMLElement | null>(null);
const tooltipRef = ref<HTMLElement | null>(null);

function measureWidth() {
  if (!rootRef.value) return;
  // 组合图表宽度跟随容器,但有最小值,避免窄屏柱条挤成线
  const w = rootRef.value.clientWidth;
  if (w > 0) width.value = Math.max(480, w);
}

let ro: ResizeObserver | null = null;
onMounted(() => {
  measureWidth();
  if (typeof ResizeObserver !== "undefined" && rootRef.value) {
    ro = new ResizeObserver(() => measureWidth());
    ro.observe(rootRef.value);
  }
});
onBeforeUnmount(() => {
  ro?.disconnect();
  cancelPendingTooltip();
});

function modelColor(model: string, models: string[]): string {
  const idx = models.indexOf(model);
  return CHART_PALETTE[idx % CHART_PALETTE.length];
}

// Intl.DateTimeFormat construction is far slower than format(); cache per
// (locale, variant) since this runs per axis label and per tooltip.
const chartDateFormatters = new Map<string, Intl.DateTimeFormat>();

function chartDateFormatter(localeTag: string, short: boolean): Intl.DateTimeFormat {
  const cacheKey = `${localeTag}:${short ? "short" : "long"}`;
  let formatter = chartDateFormatters.get(cacheKey);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(localeTag, short
      ? { month: "2-digit", day: "2-digit", timeZone: "UTC" }
      : { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
    chartDateFormatters.set(cacheKey, formatter);
  }
  return formatter;
}

function formatChartDate(value: string, short = false): string {
  const date = new Date(`${value}T00:00:00Z`);
  return chartDateFormatter(locale.value, short).format(date);
}

// --- 数据处理:按日期补零,得到连续的日期序列 ---
function padZeroDates(rows: DailyModelTokens[], days: number) {
  const map = new Map<string, Map<string, number>>();
  for (const r of rows) {
    if (!map.has(r.date)) map.set(r.date, new Map());
    const m = map.get(r.date)!;
    m.set(r.model, (m.get(r.model) ?? 0) + r.tokens);
  }
  // 生成最近 `days` 天的日期(UTC),缺失的天用空 map 填充
  const today = new Date();
  const dates: { date: string; models: Map<string, number>; total: number }[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setUTCDate(today.getUTCDate() - i);
    const ds = d.toISOString().slice(0, 10);
    const models = map.get(ds) ?? new Map<string, number>();
    let total = 0;
    models.forEach((v) => (total += v));
    dates.push({ date: ds, models, total });
  }
  return dates;
}

// 模型稳定排序(按总量 desc),保证图例顺序稳定
const sortedModels = computed(() => {
  const totals = new Map<string, number>();
  for (const r of props.data) {
    totals.set(r.model, (totals.get(r.model) ?? 0) + r.tokens);
  }
  return [...totals.keys()].sort((a, b) => (totals.get(b)! - totals.get(a)!));
});

const dates = computed(() => padZeroDates(props.data, props.days));

const totalTokens = computed(() => dates.value.reduce((sum, date) => sum + date.total, 0));
const chartDescription = computed(() => [
  t("模型：{count}", { count: sortedModels.value.length }),
  `${t("{days} 天合计", { days: props.days })} ${formatTokens(totalTokens.value)}`,
].join(t("；")));

const chartW = computed(() => Math.max(0, width.value - padL - padR));
const chartH = height - padT - padB;

const maxTokens = computed(() => {
  let m = 0;
  for (const d of dates.value) if (d.total > m) m = d.total;
  if (m === 0) m = 1; // 避免除零
  return m;
});

// 'nice' 的 Y 轴上限:向上取整到一个可读刻度
function niceCeil(v: number): number {
  if (v <= 0) return 1;
  const pow = Math.pow(10, Math.floor(Math.log10(v)));
  const n = v / pow;
  let nice: number;
  if (n <= 1) nice = 1;
  else if (n <= 2) nice = 2;
  else if (n <= 5) nice = 5;
  else nice = 10;
  return nice * pow;
}

const ceil = computed(() => niceCeil(maxTokens.value));

const yTicks = computed(() => {
  const steps = 4;
  const out: { value: number; y: number; label: string }[] = [];
  for (let i = 0; i <= steps; i++) {
    const val = (ceil.value * i) / steps;
    const y = padT + chartH - (val / ceil.value) * chartH;
    out.push({
      value: val,
      y,
      label: val < 1 ? formatTokens(0) : formatTokens(val),
    });
  }
  return out;
});

const barWidth = computed(() => {
  const n = dates.value.length || 1;
  return chartW.value / n;
});

// 每根柱子: [{model, idx, y, h, tokens}]
const bars = computed(() => {
  const models = sortedModels.value;
  const scale = chartH / ceil.value;
  return dates.value.map((d, i) => {
    let cursor = padT + chartH; // 从底往上堆
    const segments: { idx: number; model: string; y: number; h: number; tokens: number }[] = [];
    // 按 sortedModels 顺序堆叠,保证颜色块在所有柱子里对齐
    for (const model of models) {
      const tokens = d.models.get(model) ?? 0;
      if (tokens <= 0) continue;
      const h = tokens * scale;
      cursor -= h;
      segments.push({
        idx: models.indexOf(model) % CHART_PALETTE.length,
        model,
        y: cursor,
        h: Math.max(0.5, h),
        tokens,
      });
    }
    return { x: padL + barWidth.value * i, segments };
  });
});

// X 轴标签:太密时跳着显示,大约每 5~7 天一个标签
// 始终包含最后一天 (today),否则当日用量在 X 轴上看不到日期
const xLabels = computed(() => {
  const n = dates.value.length;
  if (n === 0) return [];
  // 目标最多 ~6 个标签
  const step = Math.max(1, Math.round(n / 6));
  const out: { x: number; text: string }[] = [];
  const lastIndex = n - 1;
  for (let i = 0; i < n; i += step) {
    const ds = dates.value[i].date;
    const text = formatChartDate(ds, true);
    out.push({ x: padL + barWidth.value * (i + 0.5), text });
  }
  // 确保最后一天 (today) 始终有标签,即使 step 没对齐到末尾
  if (out.length > 0 && lastIndex % step !== 0) {
    const ds = dates.value[lastIndex].date;
    out.push({ x: padL + barWidth.value * (lastIndex + 0.5), text: formatChartDate(ds, true) });
  }
  return out;
});

// --- tooltip ---

function tooltipRows(bi: number) {
  const d = dates.value[bi];
  if (!d) return [];
  const models = sortedModels.value;
  return models
    .map((model) => ({ model, tokens: d.models.get(model) ?? 0 }))
    .filter((row) => row.tokens > 0)
    .sort((a, b) => b.tokens - a.tokens)
    .map((row) => ({ ...row, color: modelColor(row.model, models) }));
}

// Precomputed per bar so render does not rebuild tooltip rows per column.
const barAriaLabels = computed(() => dates.value.map((d, bi) => {
  return [
    formatChartDate(d.date),
    t("合计 {total}", { total: formatTokens(d.total) }),
    ...tooltipRows(bi).map((row) => `${row.model} ${formatTokens(row.tokens)}`),
  ].join(t("；"));
}));

// Tooltip position is written imperatively (per rAF at most once); only the
// content is reactive, and it rebuilds only when the hovered bar changes.
interface TooltipRow { model: string; tokens: number; color: string }
const tooltipVisible = ref(false);
const tooltipContent = ref<{ title: string; total: number; rows: TooltipRow[] }>({
  title: "",
  total: 0,
  rows: [],
});

let currentBarIndex = -1;
let pendingBarIndex = -1;
let pendingX = 0;
let pendingY = 0;
let tooltipRaf = 0;

function positionTooltip() {
  const tip = tooltipRef.value;
  if (!tip || !tooltipVisible.value) return;
  const gap = 4;
  const maxX = Math.max(gap, document.documentElement.clientWidth - tip.offsetWidth - gap);
  const maxY = Math.max(gap, document.documentElement.clientHeight - tip.offsetHeight - gap);
  tip.style.left = `${Math.min(Math.max(gap, pendingX), maxX)}px`;
  tip.style.top = `${Math.min(Math.max(gap, pendingY), maxY)}px`;
}

function flushTooltip() {
  const bi = pendingBarIndex;
  const d = dates.value[bi];
  if (!d) return;
  if (bi !== currentBarIndex) {
    currentBarIndex = bi;
    tooltipContent.value = {
      title: formatChartDate(d.date),
      total: d.total,
      rows: tooltipRows(bi),
    };
    if (!tooltipVisible.value) tooltipVisible.value = true;
    // New content resizes the tooltip; measure after Vue flushes the DOM.
    void nextTick(positionTooltip);
  } else {
    positionTooltip();
  }
}

function scheduleTooltip(bi: number, x: number, y: number) {
  pendingBarIndex = bi;
  pendingX = x;
  pendingY = y;
  if (tooltipRaf !== 0) return;
  tooltipRaf = requestAnimationFrame(() => {
    tooltipRaf = 0;
    flushTooltip();
  });
}

function cancelPendingTooltip() {
  if (tooltipRaf !== 0) {
    cancelAnimationFrame(tooltipRaf);
    tooltipRaf = 0;
  }
}

function onEnter(bi: number, e: PointerEvent) {
  scheduleTooltip(bi, e.clientX + 14, e.clientY + 14);
}
function onMove(bi: number, e: PointerEvent) {
  scheduleTooltip(bi, e.clientX + 14, e.clientY + 14);
}
function onLeave() {
  cancelPendingTooltip();
  tooltipVisible.value = false;
  currentBarIndex = -1;
}

function onFocus(bi: number) {
  const bar = bars.value[bi];
  const rect = rootRef.value?.getBoundingClientRect();
  if (!bar || !rect) return;
  const scale = rect.width / width.value;
  const top = bar.segments.length > 0
    ? Math.min(...bar.segments.map((segment) => segment.y))
    : padT + chartH;
  scheduleTooltip(
    bi,
    rect.left + (bar.x + barWidth.value / 2) * scale + 14,
    rect.top + top * scale + 14,
  );
}

</script>

<style scoped>
.stacked-bar-chart {
  position: relative;
  width: 100%;
}
.chart-svg {
  display: block;
  width: 100%;
  height: auto;
}
.grid-line {
  stroke: var(--ocg-divider);
  stroke-width: 1;
  shape-rendering: crispEdges;
}
.axis-text {
  fill: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.bar-seg {
  transition: opacity 0.15s ease;
}
.bar-col:hover .bar-seg,
.bar-col:focus-visible .bar-seg {
  opacity: 0.82;
}
.bar-col:focus {
  outline: none;
}
.bar-col:focus-visible .bar-hitbox {
  stroke: var(--ocg-primary);
  stroke-width: 2;
  vector-effect: non-scaling-stroke;
}
.chart-tooltip {
  position: fixed;
  box-sizing: border-box;
  pointer-events: none;
  z-index: 5;
  min-width: 168px;
  max-width: 200px;
  padding: var(--ocg-space-sm) 10px;
  border: 1px solid var(--ocg-border);
  border-radius: 8px;
  background: var(--ocg-surface-raised);
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.12);
  font-size: var(--ocg-font-sm);
}
.tooltip-title {
  font-weight: 600;
  margin-bottom: 2px;
}
.tooltip-total {
  color: var(--ocg-subtle);
  margin-bottom: 6px;
  font-size: var(--ocg-font-xs);
}
.tooltip-row {
  display: flex;
  align-items: center;
  gap: 6px;
  line-height: 1.6;
}
.tooltip-row .dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex: 0 0 auto;
}
.tooltip-row .model {
  flex: 1 1 auto;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tooltip-row .tokens {
  flex: 0 0 auto;
  font-variant-numeric: tabular-nums;
  color: var(--ocg-muted);
}
</style>
