// Shapes of the collector's JSON contract. Every platform agent (macOS today,
// Windows / Linux / Android / TV later) speaks these same shapes, so the
// dashboard never has to learn a new payload.

export interface CpuLoad {
  one: number;
  five: number;
  fifteen: number;
}

export interface Cpu {
  total_pct: number;
  per_core_pct: number[];
  freq_mhz: number;
  cores: number;
  load: CpuLoad;
}

export interface Ram {
  used_bytes: number;
  total_bytes: number;
  pct: number;
  swap_used_bytes: number;
  swap_total_bytes: number;
}

export interface Net {
  down_bps: number;
  up_bps: number;
}

export interface Gpu {
  available: boolean;
  util_pct: number;
  renderer_pct: number;
  tiler_pct: number;
  mem_used_mb: number;
  mem_alloc_mb?: number;
  temp_c?: number | null;
}

export interface Battery {
  pct: number;
  charging: boolean;
  external_connected: boolean;
  draw_w: number | null;
  adapter_w: number | null;
  health_pct: number | null;
  cycles: number | null;
  time_remaining_min?: number | null;
}

export interface Disk {
  available: boolean;
  free_gb: number;
  total_gb: number;
  used_pct: number;
}

export interface DiskIo {
  read_bps: number | null;
  write_bps: number | null;
  read_iops?: number;
  write_iops?: number;
  available?: boolean;
}

export interface Power {
  available: boolean;
  cpu_w: number | null;
  gpu_w: number | null;
  package_w: number | null;
  thermal_pressure?: string | null;
  reason?: string | null;
}

export interface TempRow {
  label: string;
  c: number | null;
}

export interface ProcRow {
  name: string;
  cpu_pct?: number | null;
  mem_mb?: number | null;
}

export interface Pressure {
  memory?: { available?: boolean; level?: string; pct?: number };
  cpu?: { available?: boolean; level?: string; pct?: number };
}

export interface Snapshot {
  ts: string;
  age_s?: number;
  source?: string;
  cpu: Cpu;
  ram: Ram;
  net: Net;
  temps_c: TempRow[];
  gpu: Gpu;
  power: Power | null;
  battery: Battery;
  disk_io: DiskIo;
  disk: Disk;
  pressure?: Pressure;
  processes?: { by_cpu: ProcRow[]; by_mem?: ProcRow[] };
  proc_count?: number;
  boot_ts?: string;
  load?: CpuLoad;
}

export type Range = "1m" | "5m" | "15m" | "1h" | "6h" | "1d" | "7d" | "30d" | "1y";

export interface History {
  range: string;
  labels: string[];
  series: {
    cpu: number[];
    ram: number[];
    down_bps: number[];
    up_bps: number[];
  };
  temp_series: number[][];
  sensor_labels?: string[];
}

export interface ConsumerRow {
  name: string;
  cpu_pct: number | null;
  memory_mb: number;
  mem_avg_mb: number;
  procs: number;
}

export interface Consumers {
  range: string;
  rows: ConsumerRow[];
  count: number;
}

export interface Measure {
  id: string;
  label: string;
  unit: string;
  available: boolean;
  avg: number;
  max: number;
  min?: number;
  p95?: number;
  median?: number;
  samples?: number;
  window_s?: number;
  prev_avg?: number;
}

export interface Analytics {
  ts: number;
  range: string;
  measures: Measure[];
  findings?: string[];
}

export interface SensorRow {
  id: string;
  group: string;
  label: string;
  value: number | null;
  unit: string;
  available: boolean;
}

export interface SensorGroup {
  id: string;
  label: string;
  sensors: SensorRow[];
}

export interface Sensors {
  platform: string;
  count: number;
  groups: SensorGroup[];
}

export interface BatteryApi extends Partial<Battery> {
  ts?: number;
  temperature_c?: number;
}

export interface Thermal {
  source?: string;
  sample_ms?: number | null;
  sensors?: Array<{ label: string; c: number | null }>;
}
