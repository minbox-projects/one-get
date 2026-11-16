import { useEffect, useMemo, useRef, useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Checkbox } from "../../components/ui/Checkbox";
import { Dialog } from "../../components/ui/Dialog";
import { Input } from "../../components/ui/Input";
import { useI18n } from "../../i18n";
import { pickDirectory } from "../../lib/dialog";
import { addDownload } from "../../lib/downloads";
import { probeVideo, startVideoDownload } from "../../lib/video";
import type { AppError, VideoQualityPreference } from "../../types/app";
import type { HeaderPair, ProbeResult } from "../../types/downloads";
import type { VideoChoice, VideoProbeResult } from "../../types/video";
import { MissingComponentAction } from "../kernel/MissingComponentAction";
import { VideoQualityDialog } from "../video/VideoQualityDialog";

import { errorMessageKey, requiresComponentInstall } from "./errorMessages";
import type { DownloadFormErrors } from "./downloadValidation";
import { validateDownloadForm, validateUrl } from "./downloadValidation";
import { useLoadedSettings, useProbe } from "./useProbe";

export interface DownloadDefaults {
  destination: string;
  connections: number;
}

export interface BrowserPrefill {
  url: string;
  filename: string | null;
  headers: HeaderPair[];
  /** Opaque handle for cookies captured with this item, never a cookie value. */
  cookieHandle?: string | null;
}

export interface NewDownloadDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaults?: DownloadDefaults;
  onAdded?: () => void;
  prefill?: BrowserPrefill | null;
  onOpenComponents?: () => void;
}

function suggestedVideoFilename(title: string, fallback: string): string {
  const stem = title
    .replace(/[\\/:*?"<>|]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 120);
  return `${stem.length > 0 ? stem : fallback}.mp4`;
}

/**
 * Picks the preferred video choice from an ordered probe result. The highest
 * preference uses the first choice, while the MP4 preference uses the first
 * MP4-compatible choice and falls back to the highest available one.
 */
function preferredVideoChoice(
  probe: VideoProbeResult,
  preference: VideoQualityPreference | undefined,
): VideoChoice | null {
  const highest = probe.video[0] ?? null;
  if (preference !== "highest") {
    return probe.video.find((choice) => choice.mp4Compatible) ?? highest;
  }
  return highest;
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
  prefill = null,
  onOpenComponents,
}: NewDownloadDialogProps) {
  const { t, formatBytes } = useI18n();
  const { state: probeState, probe, reset: resetProbe } = useProbe();
  const { settings: dialogSettings, loadSettings } = useLoadedSettings();
  const parsingVideoRef = useRef(false);
  // A captured-cookie handle is single-use: take it for the one add request
  // that submits this prefill, then never send it again.
  const cookieHandleRef = useRef<string | null>(prefill?.cookieHandle ?? null);

  const defaultDestination = defaults?.destination ?? "";
  const defaultConnections = defaults?.connections ?? 8;

  const [url, setUrl] = useState(prefill?.url ?? "");
  const [filenameOverride, setFilenameOverride] = useState<string | null>(
    prefill?.filename ?? null,
  );
  const [destinationOverride, setDestinationOverride] = useState<string | null>(
    null,
  );
  const [connectionsOverride, setConnectionsOverride] = useState<string | null>(
    null,
  );
  const destination = destinationOverride ?? defaultDestination;
  const connections = connectionsOverride ?? String(defaultConnections);
  const [rateLimit, setRateLimit] = useState("");
  const [expectedSha256, setExpectedSha256] = useState("");
  const [headerEntries, setHeaderEntries] = useState<HeaderPair[]>(
    () => prefill?.headers ?? [],
  );
  const [headersTouched, setHeadersTouched] = useState(false);
  const headers = useMemo(() => {
    const agent = dialogSettings?.userAgent?.trim() ?? "";
    if (
      headersTouched ||
      agent.length === 0 ||
      headerEntries.some(
        ([name]) => name.trim().toLowerCase() === "user-agent",
      )
    ) {
      return headerEntries;
    }
    return [...headerEntries, ["User-Agent", agent] as HeaderPair];
  }, [dialogSettings, headerEntries, headersTouched]);
  const [cookiesOverride, setCookiesOverride] = useState<string | null>(null);
  const cookiesFile =
    cookiesOverride ?? (dialogSettings?.defaultCookiesFile?.trim() ?? "");
  const [proxyUrl, setProxyUrl] = useState("");
  const [parsingVideo, setParsingVideo] = useState(false);
  const [videoProbe, setVideoProbe] = useState<VideoProbeResult | null>(null);
  const [videoError, setVideoError] = useState<AppError | null>(null);
  const [allowHtml, setAllowHtml] = useState(false);
  const [errors, setErrors] = useState<DownloadFormErrors>({});
  const [htmlRequired, setHtmlRequired] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [pickingDirectory, setPickingDirectory] = useState(false);
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
    setVideoProbe(null);
    setVideoError(null);
    setParsingVideo(false);
  };

  const handleParseVideo = async () => {
    if (parsingVideoRef.current) {
      return;
    }

    setVideoError(null);
    const urlError = validateUrl(url);
    if (urlError !== null) {
      setErrors((current) => ({ ...current, url: urlError }));
      return;
    }

    setErrors((current) => ({ ...current, url: undefined }));
    parsingVideoRef.current = true;
    setParsingVideo(true);
    const result = await probeVideo({
      url: url.trim(),
      cookiesFile: cookiesFile.trim().length === 0 ? null : cookiesFile.trim(),
      proxyUrl: proxyUrl.trim().length === 0 ? null : proxyUrl.trim(),
    });

    if (!result.ok) {
      parsingVideoRef.current = false;
      setParsingVideo(false);
      setVideoError(result.error);
      return;
    }

    const settings = dialogSettings ?? (await loadSettings());
    const alwaysAsk = settings?.alwaysAskQuality ?? true;
    if (alwaysAsk) {
      parsingVideoRef.current = false;
      setParsingVideo(false);
      setVideoProbe(result.data);
      return;
    }

    const choice = preferredVideoChoice(
      result.data,
      settings?.videoQualityPreference,
    );
    if (choice === null) {
      parsingVideoRef.current = false;
      setParsingVideo(false);
      setVideoProbe(result.data);
      return;
    }

    const started = await startVideoDownload({
      url: url.trim(),
      filename: videoFilename,
      destination: destination.trim(),
      formatSelector: choice.formatSelector,
      audioOnly: false,
      connections: Number(connections),
      metadata: {
        title: result.data.title,
        durationSecs: result.data.durationSecs,
        uploader: result.data.uploader,
        quality: choice.label,
      },
      cookiesFile: cookiesFile.trim().length === 0 ? null : cookiesFile.trim(),
      proxyUrl: proxyUrl.trim().length === 0 ? null : proxyUrl.trim(),
    });
    parsingVideoRef.current = false;
    setParsingVideo(false);

    if (!started.ok) {
      setVideoError(started.error);
      return;
    }

    onAdded?.();
    onOpenChange(false);
  };

  const openComponents = onOpenComponents ?? (() => undefined);

  const handleBrowse = async () => {
    setPickingDirectory(true);
    const result = await pickDirectory();
    setPickingDirectory(false);
    if (result.ok) {
      if (result.data !== null) {
        setDestinationOverride(result.data);
        setErrors((current) => ({ ...current, destination: undefined }));
      }
      return;
    }
    setSubmitError(t("download.browseFailed"));
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
    if (prefill !== null && destination.trim().length === 0) {
      // A browser-captured prefill may rely on the backend's configured
      // default destination when the settings do not name one.
      delete formErrors.destination;
    }
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
    const cookieHandle = cookieHandleRef.current;
    cookieHandleRef.current = null;
    const result = await addDownload({
      url: url.trim(),
      filename: filename.trim(),
      destination:
        destination.trim().length === 0 ? null : destination.trim(),
      connections: rangePinned ? 1 : Number(connections),
      rateLimit: rateLimit.trim().length === 0 ? null : Number(rateLimit),
      expectedSha256:
        expectedSha256.trim().length === 0 ? null : expectedSha256.trim(),
      allowHtml,
      extraHeaders: normalizedHeaders,
      cookiesFile:
        cookiesFile.trim().length === 0 ? null : cookiesFile.trim(),
      cookieHandle,
    });
    setSubmitting(false);

    if (!result.ok) {
      const key = errorMessageKey(result.error.code);
      setSubmitError(
        result.error.code === "INVALID_DESTINATION"
          ? t("error.INVALID_DESTINATION")
          : key === "error.generic"
            ? t("download.addFailed")
            : t(key),
      );
      return;
    }

    onAdded?.();
    onOpenChange(false);
  };

  const videoFilename =
    filenameOverride !== null && filenameOverride.trim().length > 0
      ? filenameOverride.trim()
      : suggestedVideoFilename(
          videoProbe?.title ?? "",
          t("download.videoFallbackName"),
        );

  // The quality dialog is portaled outside this dialog's content, so Radix
  // reports its pointer interactions as outside dismissals. Keep this dialog
  // open while the quality dialog owns the interaction.
  const handleDialogOpenChange = (next: boolean) => {
    if (!next && videoProbe !== null) {
      return;
    }
    onOpenChange(next);
  };

  return (
    <>
    <Dialog
      open={open}
      onOpenChange={handleDialogOpenChange}
      title={t("download.newTitle")}
      closeLabel={t("common.close")}
    >
      <form className="og-dialog__form" noValidate onSubmit={handleSubmit}>
        {prefill ? (
          <p className="og-prefill-note">{t("download.browserPrefill")}</p>
        ) : null}

        <Input
          label={t("download.url")}
          value={url}
          inputMode="url"
          autoComplete="off"
          error={errors.url ? t(`validation.${errors.url}`) : undefined}
          onChange={(event) => handleUrlChange(event.target.value)}
        />

        <Input
          label={t("download.cookiesFile")}
          value={cookiesFile}
          autoComplete="off"
          onChange={(event) => setCookiesOverride(event.target.value)}
        />

        <Input
          label={t("download.proxyUrl")}
          value={proxyUrl}
          autoComplete="off"
          onChange={(event) => setProxyUrl(event.target.value)}
        />

        <div className="og-dialog__actions">
          <Button
            variant="secondary"
            loading={parsingVideo}
            onClick={() => void handleParseVideo()}
          >
            {t("download.parseVideo")}
          </Button>
        </div>

        {videoError ? (
          <>
            <p className="og-probe og-probe--error" role="alert">
              {t(errorMessageKey(videoError.code))}
            </p>
            {requiresComponentInstall(videoError.code) ? (
              <MissingComponentAction onOpenComponents={openComponents} />
            ) : null}
          </>
        ) : null}

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
            onChange={(event) => setDestinationOverride(event.target.value)}
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
          value={rangePinned ? "1" : connections}
          disabled={rangePinned}
          error={
            errors.connections ? t(`validation.${errors.connections}`) : undefined
          }
          onChange={(event) => setConnectionsOverride(event.target.value)}
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

        <HeaderEditor
          headers={headers}
          onChange={(next) => {
            setHeadersTouched(true);
            setHeaderEntries(next);
          }}
        />

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

    {videoProbe ? (
      <VideoQualityDialog
        open
        onOpenChange={(next) => {
          if (!next) {
            setVideoProbe(null);
          }
        }}
        url={url.trim()}
        probe={videoProbe}
        cookiesFile={cookiesFile.trim().length === 0 ? null : cookiesFile.trim()}
        proxyUrl={proxyUrl.trim().length === 0 ? null : proxyUrl.trim()}
        initialDestination={destination}
        initialFilename={videoFilename}
        onOpenComponents={openComponents}
        onStarted={() => {
          onAdded?.();
          onOpenChange(false);
        }}
      />
    ) : null}
    </>
  );
}
