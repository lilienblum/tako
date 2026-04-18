export type PatternSegment = { kind: "literal"; value: string } | { kind: "param"; name: string };

export interface CompiledPattern {
  source: string;
  segments: PatternSegment[];
  paramNames: string[];
  hasWildcard: boolean;
}

export interface PatternMatch {
  params: Record<string, string>;
}

export function compilePattern(pattern: string): CompiledPattern {
  if (pattern.length === 0) {
    throw new Error("pattern must not be empty");
  }
  if (pattern.startsWith("/")) {
    throw new Error("pattern must not start with '/'");
  }
  if (pattern.endsWith("/")) {
    throw new Error("pattern must not end with '/'");
  }

  const rawSegments = pattern.split("/");
  const segments: PatternSegment[] = [];
  const paramNames: string[] = [];
  let hasWildcard = false;

  if (rawSegments[0]?.startsWith(":")) {
    throw new Error("pattern must begin with a literal segment, not a param");
  }

  for (let i = 0; i < rawSegments.length; i++) {
    const raw = rawSegments[i] ?? "";
    if (raw.length === 0) {
      throw new Error(`pattern contains empty segment at index ${i}`);
    }
    if (raw === "*") {
      if (i !== rawSegments.length - 1) {
        throw new Error("wildcard must be the final segment");
      }
      hasWildcard = true;
      continue;
    }
    if (raw.startsWith(":")) {
      const name = raw.slice(1);
      if (name.length === 0 || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
        throw new Error(`param name must begin with a letter or underscore (got ':${name}')`);
      }
      if (paramNames.includes(name)) {
        throw new Error(`duplicate param name ':${name}'`);
      }
      paramNames.push(name);
      segments.push({ kind: "param", name });
      continue;
    }
    segments.push({ kind: "literal", value: raw });
  }

  return { source: pattern, segments, paramNames, hasWildcard };
}

export function matchPattern(pattern: CompiledPattern, channel: string): PatternMatch | null {
  const parts = channel.split("/");
  if (pattern.hasWildcard) {
    if (parts.length < pattern.segments.length + 1) return null;
  } else if (parts.length !== pattern.segments.length) {
    return null;
  }

  const params: Record<string, string> = {};
  for (let i = 0; i < pattern.segments.length; i++) {
    const seg = pattern.segments[i]!;
    const part = parts[i]!;
    if (part.length === 0) return null;
    if (seg.kind === "literal") {
      if (seg.value !== part) return null;
    } else {
      params[seg.name] = part;
    }
  }
  return { params };
}

/**
 * Specificity comparator: negative means `a` is MORE specific (should match first).
 * Rules:
 *   1. Literal segments beat param segments beat wildcard-only tails (per-position).
 *   2. If per-position scores tie, longer pattern (more segments) wins.
 *   3. Otherwise zero (ties are resolved by discovery order upstream).
 */
export function comparePatterns(a: CompiledPattern, b: CompiledPattern): number {
  const len = Math.max(a.segments.length, b.segments.length);
  for (let i = 0; i < len; i++) {
    const sa = a.segments[i];
    const sb = b.segments[i];
    const ra = segmentRank(sa);
    const rb = segmentRank(sb);
    if (ra !== rb) return ra - rb;
  }
  const wa = a.hasWildcard ? 1 : 0;
  const wb = b.hasWildcard ? 1 : 0;
  if (wa !== wb) return wa - wb;
  if (a.segments.length !== b.segments.length) {
    return b.segments.length - a.segments.length;
  }
  return 0;
}

function segmentRank(seg: PatternSegment | undefined): number {
  if (seg === undefined) return 3;
  if (seg.kind === "literal") return 0;
  return 1;
}
