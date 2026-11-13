const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

const BYTE_STEP = 1024;

export type Locale = "zh-CN" | "en-US";

function isKnownNumber(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function isKnownTime(
  value: string | number | Date | null | undefined,
): value is string | number | Date {
  if (value === null || value === undefined) {
    return false;
  }
  if (typeof value === "string" && value.trim().length === 0) {
    return false;
  }
  if (typeof value === "number" && !Number.isFinite(value)) {
    return false;
  }
  return true;
}

function formatDecimal(locale: Locale, value: number, maximumFractionDigits: number) {
  return new Intl.NumberFormat(locale, { maximumFractionDigits }).format(value);
}

/** Formats an integer or decimal using locale-aware grouping. */
export function formatNumber(
  value: number | null | undefined,
  locale: Locale,
): string | null {
  if (!isKnownNumber(value)) {
    return null;
  }
  return formatDecimal(locale, value, 2);
}

/** Formats a byte count with binary unit steps and a locale-aware number. */
export function formatBytes(
  value: number | null | undefined,
  locale: Locale,
): string | null {
  if (!isKnownNumber(value)) {
    return null;
  }

  if (value < BYTE_STEP) {
    return `${formatDecimal(locale, value, 0)} ${BYTE_UNITS[0]}`;
  }

  const exponent = Math.min(
    Math.floor(Math.log(value) / Math.log(BYTE_STEP)),
    BYTE_UNITS.length - 1,
  );
  const scaled = value / BYTE_STEP ** exponent;
  const digits = scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2;
  return `${formatDecimal(locale, scaled, digits)} ${BYTE_UNITS[exponent]}`;
}

/** Formats a bytes-per-second rate using the byte formatter. */
export function formatSpeed(
  value: number | null | undefined,
  locale: Locale,
): string | null {
  const bytes = formatBytes(value, locale);
  return bytes === null ? null : `${bytes}/s`;
}

function formatUnit(
  locale: Locale,
  value: number,
  unit: "second" | "minute" | "hour",
): string {
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit,
    unitDisplay: "short",
    maximumFractionDigits: 0,
  }).format(value);
}

/** Formats a remaining duration in seconds, or returns null when unknown. */
export function formatEta(
  value: number | null | undefined,
  locale: Locale,
): string | null {
  if (!isKnownNumber(value)) {
    return null;
  }

  const total = Math.round(value);
  if (total < 60) {
    return formatUnit(locale, total, "second");
  }
  if (total < 3600) {
    const minutes = Math.floor(total / 60);
    const seconds = total % 60;
    return seconds === 0
      ? formatUnit(locale, minutes, "minute")
      : `${formatUnit(locale, minutes, "minute")} ${formatUnit(locale, seconds, "second")}`;
  }

  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  return minutes === 0
    ? formatUnit(locale, hours, "hour")
    : `${formatUnit(locale, hours, "hour")} ${formatUnit(locale, minutes, "minute")}`;
}

/** Formats a timestamp or ISO string with a locale-aware date and time. */
export function formatDate(
  value: string | number | Date | null | undefined,
  locale: Locale,
): string | null {
  if (!isKnownTime(value)) {
    return null;
  }

  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) {
    return null;
  }

  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}
