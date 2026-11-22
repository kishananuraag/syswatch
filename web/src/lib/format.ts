// Number formatting shared by every screen: tabular, one decimal by default,
// an em dash whenever the machine did not report the value.
export function isNum(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

export function fmt(v: unknown, decimals = 1): string {
  if (!isNum(v)) return "—";
  return v.toFixed(decimals);
}

export function gb(bytes: unknown): number | null {
  return isNum(bytes) ? bytes / 1073741824 : null;
}

export function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}
