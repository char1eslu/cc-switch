import type { UsageData } from "@/types";

interface UsageSummaryLabels {
  invalid: string;
  remaining: string;
  used: string;
}

type RelativeTimeTranslator = (
  key: string,
  options?: { count?: number },
) => string;

export function formatUsageRelativeTime(
  timestamp: number,
  now: number,
  t: RelativeTimeTranslator,
): string {
  const diff = Math.max(0, Math.floor((now - timestamp) / 1000));

  if (diff < 60) return t("usage.justNow");
  if (diff < 3600)
    return t("usage.minutesAgo", { count: Math.floor(diff / 60) });
  if (diff < 86400)
    return t("usage.hoursAgo", { count: Math.floor(diff / 3600) });
  return t("usage.daysAgo", { count: Math.floor(diff / 86400) });
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? value.toString() : value.toFixed(2);
}

function formatValue(value: number, unit?: string): string {
  if (!unit) {
    return formatNumber(value);
  }

  return unit === "%"
    ? `${formatNumber(value)}%`
    : `${formatNumber(value)} ${unit}`;
}

function isNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function formatUsed(
  data: UsageData,
  labels: UsageSummaryLabels,
): string | null {
  if (!isNumber(data.used)) {
    return null;
  }

  if (isNumber(data.total) && data.total > 0) {
    const usedPercent = (data.used / data.total) * 100;

    if (data.unit === "%" && data.total === 100) {
      return `${labels.used} ${formatValue(data.used, "%")}`;
    }

    return `${labels.used} ${formatNumber(usedPercent)}%`;
  }

  return `${labels.used} ${formatValue(data.used, data.unit)}`;
}

export function formatUsageDataSummary(
  data: UsageData,
  labels: UsageSummaryLabels,
): string {
  const planPrefix = data.planName ? `[${data.planName}] ` : "";

  if (data.isValid === false) {
    return `${planPrefix}${data.invalidMessage || labels.invalid}`;
  }

  const parts = [
    formatUsed(data, labels),
    isNumber(data.remaining)
      ? `${labels.remaining} ${formatValue(data.remaining, data.unit)}`
      : null,
    data.extra || null,
  ].filter((part): part is string => Boolean(part));

  return `${planPrefix}${parts.join(" / ") || labels.invalid}`;
}
