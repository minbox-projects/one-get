import type { DownloadSummary } from "../../types/downloads";

/** Returns only the host of a URL, or null when it cannot be parsed. */
export function domainFromUrl(url: string): string | null {
  try {
    const hostname = new URL(url).hostname;
    return hostname.length > 0 ? hostname : null;
  } catch {
    return null;
  }
}

/** Computes task progress as a percentage, or null when the total is unknown. */
export function progressPercent(task: DownloadSummary): number | null {
  if (task.state === "completed") {
    return 100;
  }

  if (task.totalBytes === null || task.totalBytes <= 0) {
    return null;
  }

  const percent = (task.downloadedBytes / task.totalBytes) * 100;
  return Math.min(100, Math.max(0, percent));
}
