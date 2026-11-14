import { useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { Input } from "../../components/ui/Input";
import { useI18n } from "../../i18n";
import { startVideoDownload } from "../../lib/video";
import type { AppError } from "../../types/app";
import type { VideoChoice, VideoProbeResult } from "../../types/video";
import { MissingComponentAction } from "../kernel/MissingComponentAction";

import { errorMessageKey, requiresComponentInstall } from "../downloads/errorMessages";
import { validateFilename } from "../downloads/downloadValidation";

export interface VideoQualityDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  url: string;
  probe: VideoProbeResult;
  cookiesFile: string | null;
  proxyUrl: string | null;
  initialDestination: string;
  initialFilename: string;
  onOpenComponents: () => void;
  onStarted?: () => void;
}

interface Selection {
  kind: "video" | "audio";
  index: number;
}

interface FieldErrors {
  filename?: string;
  destination?: string;
}

function initialSelection(probe: VideoProbeResult): Selection | null {
  if (probe.video.length > 0) {
    const index =
      probe.defaultIndex !== null &&
      probe.defaultIndex >= 0 &&
      probe.defaultIndex < probe.video.length
        ? probe.defaultIndex
        : 0;
    return { kind: "video", index };
  }
  return probe.audio.length > 0 ? { kind: "audio", index: 0 } : null;
}

function selectedChoice(
  probe: VideoProbeResult,
  selection: Selection | null,
): VideoChoice | null {
  if (selection === null) {
    return null;
  }
  const list = selection.kind === "video" ? probe.video : probe.audio;
  return list[selection.index] ?? null;
}

/** Metadata, quality choices, and start validation for one probed video. */
export function VideoQualityDialog({
  open,
  onOpenChange,
  url,
  probe,
  cookiesFile,
  proxyUrl,
  initialDestination,
  initialFilename,
  onOpenComponents,
  onStarted,
}: VideoQualityDialogProps) {
  const { t, formatBytes, formatEta } = useI18n();
  const [selection, setSelection] = useState<Selection | null>(() =>
    initialSelection(probe),
  );
  const [filename, setFilename] = useState(initialFilename);
  const [destination, setDestination] = useState(initialDestination);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [submitting, setSubmitting] = useState(false);
  const [startError, setStartError] = useState<AppError | null>(null);

  const unknown = t("common.unknown");
  const duration =
    probe.durationSecs === null
      ? unknown
      : formatEta(probe.durationSecs) ?? unknown;
  const uploader = probe.uploader ?? unknown;
  const choice = selectedChoice(probe, selection);

  const handleVideoChange = (index: number) => {
    setSelection({ kind: "video", index });
  };

  const handleAudioChange = (index: number) => {
    setSelection({ kind: "audio", index });
  };

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (submitting) {
      return;
    }

    const filenameError = validateFilename(filename);
    const nextErrors: FieldErrors = {};
    if (filenameError !== null) {
      nextErrors.filename = t(`validation.${filenameError}`);
    }
    if (destination.trim().length === 0) {
      nextErrors.destination = t("validation.destination_required");
    }
    setErrors(nextErrors);
    setStartError(null);

    if (Object.keys(nextErrors).length > 0 || choice === null) {
      return;
    }

    setSubmitting(true);
    const result = await startVideoDownload({
      url,
      filename: filename.trim(),
      destination: destination.trim(),
      formatSelector: choice.formatSelector,
      audioOnly: selection?.kind === "audio",
      metadata: {
        title: probe.title,
        durationSecs: probe.durationSecs,
        uploader: probe.uploader,
        quality: choice.label,
      },
      cookiesFile,
      proxyUrl,
    });
    setSubmitting(false);

    if (!result.ok) {
      setStartError(result.error);
      return;
    }

    onStarted?.();
    onOpenChange(false);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("video.qualityTitle")}
      closeLabel={t("common.close")}
    >
      <form className="og-dialog__form" noValidate onSubmit={handleSubmit}>
        <div className="og-probe">
          <p>{probe.title}</p>
          <p>{uploader}</p>
          <p>
            {t("video.duration")}: {duration}
          </p>
        </div>

        <fieldset className="og-field">
          <legend className="og-field__label">
            {t("video.videoChoices")}
          </legend>
          {probe.video.map((item, index) => (
            <label key={item.id} className="og-checkbox-field">
              <input
                type="radio"
                name="video-quality"
                className="og-choice__radio"
                checked={
                  selection?.kind === "video" && selection.index === index
                }
                onChange={() => handleVideoChange(index)}
              />
              <span className="og-checkbox__label">
                <span>{item.label}</span>
                {" · "}
                <span>{formatBytes(item.sizeBytes) ?? unknown}</span>
                {item.mp4Compatible ? <span> · {t("video.mp4")}</span> : null}
              </span>
            </label>
          ))}
        </fieldset>

        <fieldset className="og-field">
          <legend className="og-field__label">
            {t("video.audioChoices")}
          </legend>
          {probe.audio.map((item, index) => (
            <label key={item.id} className="og-checkbox-field">
              <input
                type="radio"
                name="video-quality"
                className="og-choice__radio"
                checked={
                  selection?.kind === "audio" && selection.index === index
                }
                onChange={() => handleAudioChange(index)}
              />
              <span className="og-checkbox__label">
                <span>{item.label}</span>
                {" · "}
                <span>{formatBytes(item.sizeBytes) ?? unknown}</span>
                {item.mp4Compatible ? <span> · {t("video.mp4")}</span> : null}
              </span>
            </label>
          ))}
        </fieldset>

        <Input
          label={t("download.filename")}
          value={filename}
          autoComplete="off"
          error={errors.filename}
          onChange={(event) => {
            setFilename(event.target.value);
            setErrors((current) => ({ ...current, filename: undefined }));
          }}
        />

        <Input
          label={t("download.destination")}
          value={destination}
          autoComplete="off"
          error={errors.destination}
          onChange={(event) => {
            setDestination(event.target.value);
            setErrors((current) => ({ ...current, destination: undefined }));
          }}
        />

        {startError ? (
          <>
            <p className="og-probe og-probe--error" role="alert">
              {t(errorMessageKey(startError.code))}
            </p>
            {requiresComponentInstall(startError.code) ? (
              <MissingComponentAction onOpenComponents={onOpenComponents} />
            ) : null}
          </>
        ) : null}

        <div className="og-dialog__actions">
          <Button
            variant="secondary"
            onClick={() => onOpenChange(false)}
          >
            {t("common.cancel")}
          </Button>
          <Button type="submit" loading={submitting}>
            {t("video.start")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
