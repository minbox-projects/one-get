export interface OneGetMarkProps {
  readonly className?: string;
}

/**
 * Renders the OneGet brand mark as a decorative inline glyph.
 *
 * The mark inherits the surrounding text color and is hidden from assistive
 * technology so it never alters the accessible name of its heading.
 */
export function OneGetMark({ className }: OneGetMarkProps) {
  return (
    <svg
      className={className}
      viewBox="0 0 128 128"
      fill="none"
      aria-hidden="true"
      focusable="false"
    >
      <g
        fill="none"
        stroke="currentColor"
        strokeWidth="28"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M64 22 V50" />
        <path d="M40 46 L64 68 L88 46" />
      </g>
      <circle cx="64" cy="106" r="14" fill="currentColor" />
    </svg>
  );
}
