/** Accept RFC 3339 and the legacy Rust time tuple during the backend rollout. */
export function parseTimestamp(value: string | number[]): Date {
  if (
    Array.isArray(value) &&
    value.length === 9 &&
    value.every(Number.isFinite)
  ) {
    const [
      year,
      ordinal,
      hour,
      minute,
      second,
      nanos,
      offsetHour,
      offsetMinute,
      offsetSecond,
    ] = value;
    return new Date(
      Date.UTC(
        year,
        0,
        ordinal,
        hour - offsetHour,
        minute - offsetMinute,
        second - offsetSecond,
        nanos / 1_000_000,
      ),
    );
  }
  return new Date(typeof value === "string" ? value : NaN);
}
