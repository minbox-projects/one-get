/* eslint-disable react-refresh/only-export-components -- The batch parser is exported beside the dialog for direct testing. */
import { useId, useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { Input } from "../../components/ui/Input";
import { useI18n } from "../../i18n";
import { pickDirectory } from "../../lib/dialog";
import { addDownload } from "../../lib/downloads";
import type { HeaderPair } from "../../types/downloads";

import type {
  DownloadFormErrors,
  ValidationCode,
} from "./downloadValidation";
import {
  validateConnections,
  validateHeaders,
  validateRateLimit,
  validateUrl,
} from "./downloadValidation";
import type { DownloadDefaults } from "./NewDownloadDialog";
import { HeaderEditor } from "./NewDownloadDialog";

export interface BatchEntry {
  line: number;
  url: string;
}

export interface BatchInvalid {
  line: number;
  code: ValidationCode;
}

export interface BatchDuplicate {
  line: number;
  url: string;
}

export interface BatchParseResult {
  entries: BatchEntry[];
  invalid: BatchInvalid[];
  duplicates: BatchDuplicate[];
}

export interface BatchDownloadDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaults?: DownloadDefaults;
  onAdded?: () => void;
}

/** Parses one URL per line, reporting invalid and in-batch duplicate lines. */
export function parseBatchUrls(input: string): BatchParseResult {
  const entries: BatchEntry[] = [];
  const invalid: BatchInvalid[] = [];
  const duplicates: BatchDuplicate[] = [];
  const seen = new Set<string>();

  input.split(/\r?\n/).forEach((raw, index) => {
    const url = raw.trim();
    if (url.length === 0) {
      return;
    }

    const line = index + 1;
    const code = validateUrl(url);
    if (code !== null) {
      invalid.push({ line, code });
      return;
    }

    if (seen.has(url)) {
      duplicates.push({ line, url });
      return;
    }

    seen.add(url);
    entries.push({ line, url });
  });

  return { entries, invalid, duplicates };
}

function activeHeaders(headers: readonly HeaderPair[]): HeaderPair[] {
  return headers
    .filter(([name, value]) => name.trim().length > 0 || value.length > 0)
    .map(([name, value]) => [name.trim(), value]);
}

function filenameFromUrl(url: string): string {
  try {
    const segments = new URL(url).pathname.split("/").filter(Boolean);
    const last = segments[segments.length - 1];
    if (last) {
      return decodeURIComponent(last);
    }
  } catch {
    // Fall through to the generic filename below.
  }
  return "download";
}

/** Controlled batch form sharing destination, connections and header options. */
export function BatchDownloadDialog({
  open,
  onOpenChange,
  defaults,
  onAdded,
}: BatchDownloadDialogProps) {
  const { t } = useI18n();
  const textareaId = useId();

  const defaultDestination = defaults?.destination ?? "";
  const defaultConnections = defaults?.connections ?? 8;

  const [urls, setUrls] = useState("");
  const [destination, setDestination] = useState(defaultDestination);
  const [connections, setConnections] = useState(String(defaultConnections));
  const [rateLimit, setRateLimit] = useState("");
  const [headers, setHeaders] = useState<HeaderPair[]>([]);
  const [errors, setErrors] = useState<DownloadFormErrors>({});
  const [parsed, setParsed] = useState<BatchParseResult | null>(null);
  const [summary, setSummary] = useState<{
    accepted: number;
    rejected: number;
  } | null>(null);
  const [batchError, setBatchError] = useState<string | null>(null);
  const [pickingDirectory, setPickingDirectory] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const handleBrowse = async () => {
    setPickingDirectory(true);
    const result = await pickDirectory();
    setPickingDirectory(false);
    if (result.ok) {
      if (result.data !== null) {
        setDestination(result.data);
        setErrors((current) => ({ ...current, destination: undefined }));
      }
      return;
    }
    setBatchError(t("download.browseFailed"));
  };

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setBatchError(null);
    setErrors({});

    const result = parseBatchUrls(urls);
    setParsed(result);

    if (result.entries.length === 0) {
      setBatchError(t("batch.empty"));
      return;
    }

    const normalizedHeaders = activeHeaders(headers);
    const nextErrors: DownloadFormErrors = {};
    if (destination.trim().length === 0) {
      nextErrors.destination = "destination_required";
    }
    const connectionCode = validateConnections(connections);
    if (connectionCode !== null) {
      nextErrors.connections = connectionCode;
    }
    const rateCode = validateRateLimit(rateLimit);
    if (rateCode !== null) {
      nextErrors.rateLimit = rateCode;
    }
    const headerCode = validateHeaders(normalizedHeaders);
    if (headerCode !== null) {
      nextErrors.headers = headerCode;
    }
    setErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) {
      return;
    }

    setSubmitting(true);
    let accepted = 0;
    for (const entry of result.entries) {
      const added = await addDownload({
        url: entry.url,
        filename: filenameFromUrl(entry.url),
        destination: destination.trim(),
        connections: Number(connections),
        rateLimit: rateLimit.trim().length === 0 ? null : Number(rateLimit),
        expectedSha256: null,
        allowHtml: false,
        extraHeaders: normalizedHeaders,
      });
      if (added.ok) {
        accepted += 1;
      }
    }
    setSubmitting(false);

    const rejected =
      result.invalid.length +
      result.duplicates.length +
      (result.entries.length - accepted);
    setSummary({ accepted, rejected });

    if (accepted > 0) {
      onAdded?.();
    }
  };

  const lineReason = (code: ValidationCode) => t(`validation.${code}`);

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("batch.title")}
      closeLabel={t("common.close")}
    >
      <form className="og-dialog__form" noValidate onSubmit={handleSubmit}>
        <div className="og-field">
          <label className="og-field__label" htmlFor={textareaId}>
            {t("batch.urls")}
          </label>
          <textarea
            id={textareaId}
            className="og-input og-textarea"
            value={urls}
            rows={5}
            spellCheck={false}
            aria-invalid={batchError ? true : undefined}
            onChange={(event) => setUrls(event.target.value)}
          />
        </div>

        <div className="og-field-with-action">
          <Input
            label={t("download.destination")}
            value={destination}
            autoComplete="off"
            error={
              errors.destination
                ? t(`validation.${errors.destination}`)
                : undefined
            }
            onChange={(event) => setDestination(event.target.value)}
          />
          <Button
            variant="secondary"
            loading={pickingDirectory}
            onClick={() => void handleBrowse()}
          >
            {t("download.browse")}
          </Button>
        </div>

        <Input
          label={t("download.connections")}
          type="number"
          min={1}
          max={64}
          value={connections}
          error={
            errors.connections ? t(`validation.${errors.connections}`) : undefined
          }
          onChange={(event) => setConnections(event.target.value)}
        />

        <Input
          label={t("download.rateLimit")}
          type="number"
          min={1}
          value={rateLimit}
          error={
            errors.rateLimit ? t(`validation.${errors.rateLimit}`) : undefined
          }
          onChange={(event) => setRateLimit(event.target.value)}
        />

        <HeaderEditor headers={headers} onChange={setHeaders} />

        {errors.headers ? (
          <p className="og-field__error" role="alert">
            {t(`validation.${errors.headers}`)}
          </p>
        ) : null}

        {batchError ? (
          <p className="og-probe og-probe--error" role="alert">
            {batchError}
          </p>
        ) : null}

        {parsed !== null ? (
          <div className="og-batch-result">
            {parsed.invalid.length > 0 || parsed.duplicates.length > 0 ? (
              <ul className="og-batch-result__lines">
                {parsed.invalid.map((item) => (
                  <li key={`invalid-${item.line}`}>
                    {t("batch.lineInvalid", {
                      line: item.line,
                      reason: lineReason(item.code),
                    })}
                  </li>
                ))}
                {parsed.duplicates.map((item) => (
                  <li key={`duplicate-${item.line}`}>
                    {t("batch.lineDuplicate", { line: item.line })}
                  </li>
                ))}
              </ul>
            ) : null}
            {summary ? (
              <p className="og-batch-result__summary" role="status">
                {t("batch.summary", {
                  accepted: summary.accepted,
                  rejected: summary.rejected,
                })}
              </p>
            ) : null}
          </div>
        ) : null}

        <div className="og-dialog__actions">
          <Button type="submit" loading={submitting}>
            {t("batch.start")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
