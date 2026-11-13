import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Checkbox } from "../../components/ui/Checkbox";
import { Dialog } from "../../components/ui/Dialog";
import { Input } from "../../components/ui/Input";
import { useI18n } from "../../i18n";
import { addDownload } from "../../lib/downloads";
import type { HeaderPair, ProbeResult } from "../../types/downloads";

import type { DownloadFormErrors } from "./downloadValidation";
import { validateDownloadForm, validateUrl } from "./downloadValidation";
import { useProbe } from "./useProbe";

export interface DownloadDefaults {
  destination: string;
  connections: number;
}

export interface NewDownloadDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaults?: DownloadDefaults;
  onAdded?: () => void;
}

export interface HeaderEditorProps {
  headers: HeaderPair[];
  onChange: (headers: HeaderPair[]) => void;
}

function activeHeaders(headers: readonly HeaderPair[]): HeaderPair[] {
  return headers
    .filter(([name, value]) => name.trim().length > 0 || value.length > 0)
    .map(([name, value]) => [name.trim(), value]);
}

function isHtmlProbe(result: ProbeResult): boolean {
  if ((result.contentType ?? "").toLowerCase().includes("text/html")) {
    return true;
  }
  return /\.html?$/i.test(result.filename);
}

function probeErrorMessage(
  code: string,
  t: (key: string) => string,
): string {
  const key = `probe.error.${code}`;
  const message = t(key);
  return message === key ? t("download.probeFailed") : message;
}

/** Editor for safe custom request headers shared by both download dialogs. */
export function HeaderEditor({ headers, onChange }: HeaderEditorProps) {
  const { t } = useI18n();

  const update = (index: number, pair: HeaderPair) => {
    onChange(headers.map((current, position) => (position === index ? pair : current)));
  };

  return (
    <details className="og-advanced">
      <summary className="og-advanced__summary">
        {t("download.advancedHeaders")}
      </summary>
      <div className="og-advanced__body">
        {headers.map(([name, value], index) => (
          <div className="og-header-row" key={index}>
            <Input
              label={t("download.headerName")}
              value={name}
              onChange={(event) => update(index, [event.target.value, value])}
            />
            <Input
              label={t("download.headerValue")}
              value={value}
              onChange={(event) => update(index, [name, event.target.value])}
            />
            <Button
              variant="ghost"
              aria-label={t("download.removeHeader")}
              onClick={() =>
                onChange(headers.filter((_, position) => position !== index))
              }
            >
              <span aria-hidden="true">&times;</span>
            </Button>
          </div>
        ))}
        <Button
          variant="secondary"
          onClick={() => onChange([...headers, ["", ""]])}
        >
          {t("download.addHeader")}
        </Button>
      </div>
    </details>
  );
}

/** Controlled new-download form with probe, validation and HTML allowance. */
export function NewDownloadDialog({
  open,
  onOpenChange,
  defaults,
  onAdded,
}: NewDownloadDialogProps) {
  const { t, formatBytes } = useI18n();
  const { state: probeState, probe, reset: resetProbe } = useProbe();

  const defaultDestination = defaults?.destination ?? "";
  const defaultConnections = defaults?.connections ?? 8;

  const [url, setUrl] = useState("");
  const [filenameOverride, setFilenameOverride] = useState<string | null>(null);
  const [destination, setDestination] = useState(defaultDestination);
  const [connections, setConnections] = useState(String(defaultConnections));
  const [rateLimit, setRateLimit] = useState("");
  const [expectedSha256, setExpectedSha256] = useState("");
  const [headers, setHeaders] = useState<HeaderPair[]>([]);
  const [allowHtml, setAllowHtml] = useState(false);
  const [errors, setErrors] = useState<DownloadFormErrors>({});
  const [htmlRequired, setHtmlRequired] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!open) {
      return;
    }
    const candidate = url.trim();
    if (candidate.length === 0 || validateUrl(candidate) !== null) {
      resetProbe();
      return;
    }

    const timer = setTimeout(() => {
      void probe(candidate, allowHtml, activeHeaders(headers));
    }, 250);
    return () => clearTimeout(timer);
  }, [open, url, allowHtml, headers, probe, resetProbe]);

  const htmlResult =
    probeState.status === "success" ? isHtmlProbe(probeState.result) : false;
  const rangePinned =
    probeState.status === "success" && !probeState.result.rangeSupported;
  const filename =
    filenameOverride ??
    (probeState.status === "success" ? probeState.result.filename : "");

  const handleUrlChange = (value: string) => {
    setUrl(value);
    setFilenameOverride(null);
    setAllowHtml(false);
    setHtmlRequired(false);
    setSubmitError(null);
  };

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setSubmitError(null);
    setHtmlRequired(false);

    const normalizedHeaders = activeHeaders(headers);
    const formErrors = validateDownloadForm({
      url,
      filename,
      destination,
      connections,
      rateLimit,
      expectedSha256,
      allowHtml,
      headers: normalizedHeaders,
    });
    setErrors(formErrors);

    if (htmlResult && !allowHtml) {
      setHtmlRequired(true);
    }
    if (Object.keys(formErrors).length > 0 || (htmlResult && !allowHtml)) {
      return;
    }
    if (probeState.status === "error") {
      return;
    }

    setSubmitting(true);
    const result = await addDownload({
      url: url.trim(),
      filename: filename.trim(),
      destination: destination.trim(),
      connections: rangePinned ? 1 : Number(connections),
      rateLimit: rateLimit.trim().length === 0 ? null : Number(rateLimit),
      expectedSha256:
        expectedSha256.trim().length === 0 ? null : expectedSha256.trim(),
      allowHtml,
      extraHeaders: normalizedHeaders,
    });
    setSubmitting(false);

    if (!result.ok) {
      setSubmitError(t("download.addFailed"));
      return;
    }

    onAdded?.();
    onOpenChange(false);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("download.newTitle")}
      closeLabel={t("common.close")}
    >
      <form className="og-dialog__form" noValidate onSubmit={handleSubmit}>
        <Input
          label={t("download.url")}
          value={url}
          inputMode="url"
          autoComplete="off"
          error={errors.url ? t(`validation.${errors.url}`) : undefined}
          onChange={(event) => handleUrlChange(event.target.value)}
        />

        {probeState.status === "detecting" ? (
          <p className="og-probe" role="status">
            {t("download.probing")}
          </p>
        ) : null}

        {probeState.status === "error" ? (
          <p className="og-probe og-probe--error" role="alert">
            {probeErrorMessage(probeState.error.code, t)}
          </p>
        ) : null}

        {probeState.status === "success" ? (
          <div className="og-probe">
            <p>
              {t("download.probeFilename", {
                filename: probeState.result.filename,
              })}
            </p>
            <p>
              {probeState.result.totalBytes === null
                ? t("download.probeUnknownSize")
                : t("download.probeSize", {
                    size:
                      formatBytes(probeState.result.totalBytes) ??
                      t("common.unknown"),
                  })}
            </p>
            <p>
              {probeState.result.rangeSupported
                ? t("download.probeRange")
                : t("download.probeNoRange")}
            </p>
          </div>
        ) : null}

        <Input
          label={t("download.filename")}
          value={filename}
          autoComplete="off"
          error={errors.filename ? t(`validation.${errors.filename}`) : undefined}
          onChange={(event) => setFilenameOverride(event.target.value)}
        />

        <Input
          label={t("download.destination")}
          value={destination}
          autoComplete="off"
          error={
            errors.destination ? t(`validation.${errors.destination}`) : undefined
          }
          onChange={(event) => setDestination(event.target.value)}
        />

        <Input
          label={t("download.connections")}
          type="number"
          min={1}
          max={64}
          value={rangePinned ? "1" : connections}
          disabled={rangePinned}
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

        <Input
          label={t("download.expectedSha256")}
          value={expectedSha256}
          autoComplete="off"
          spellCheck={false}
          error={
            errors.expectedSha256
              ? t(`validation.${errors.expectedSha256}`)
              : undefined
          }
          onChange={(event) => setExpectedSha256(event.target.value)}
        />

        {htmlResult ? (
          <>
            <p className="og-probe og-probe--warning">{t("download.probeHtml")}</p>
            <Checkbox
              label={t("download.allowHtml")}
              checked={allowHtml}
              error={htmlRequired ? t("download.htmlRequired") : undefined}
              onChange={(event) => {
                setAllowHtml(event.target.checked);
                if (event.target.checked) {
                  setHtmlRequired(false);
                }
              }}
            />
          </>
        ) : null}

        <HeaderEditor headers={headers} onChange={setHeaders} />

        {errors.headers ? (
          <p className="og-field__error" role="alert">
            {t(`validation.${errors.headers}`)}
          </p>
        ) : null}

        {submitError ? (
          <p className="og-probe og-probe--error" role="alert">
            {submitError}
          </p>
        ) : null}

        <div className="og-dialog__actions">
          <Button type="submit" loading={submitting}>
            {t("download.start")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
