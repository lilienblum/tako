/**
 * Exponential backoff with +/- 20% jitter.
 *
 * `attempts` is the number of attempts that have already completed (>= 1).
 * The first retry (after attempt 1) waits ~base, the next ~2*base, etc.,
 * capped at `max`.
 */
export function expBackoffMs(
  attempts: number,
  base = 1_000,
  max = 3_600_000,
  jitterPct = 0.2,
): number {
  const safeAttempts = Math.max(1, attempts);
  const exp = Math.min(base * 2 ** (safeAttempts - 1), max);
  const jitter = exp * jitterPct * (Math.random() * 2 - 1);
  return Math.max(0, Math.round(exp + jitter));
}
