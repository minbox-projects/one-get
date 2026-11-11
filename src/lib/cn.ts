export type ClassNameValue = string | number | false | null | undefined;

/**
 * Joins the truthy class name values with a single space.
 */
export function cn(...values: ClassNameValue[]): string {
  return values
    .filter(
      (value): value is string | number =>
        value !== false && value !== null && value !== undefined && value !== "",
    )
    .join(" ");
}
