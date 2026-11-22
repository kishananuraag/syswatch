// Overview: the KPI grid, then the apps and status panels, then the hint line.
import type { BatteryApi, Consumers, History, Range, Sensors, Snapshot, Thermal } from "../data/types";
import { statusRows, type AppRow, type LiveSeries, type TileId } from "../lib/derive";
import { AppsPanel, StatusPanel } from "./Panels";
import { Tiles } from "./Tiles";

interface Props {
  current: Snapshot;
  history: History;
  consumers: Consumers;
  sensors: Sensors | null;
  thermal: Thermal | null;
  batteryApi: BatteryApi;
  range: Range;
  live: LiveSeries;
  focused: TileId | null;
  onFocus: (id: TileId) => void;
  ageSeconds: number | null;
  tiles: TileId[];
  apps: AppRow[];
  onOpenApps: () => void;
  hidden?: boolean;
}

export function Overview(props: Props) {
  const status = statusRows({
    current: props.current,
    consumers: props.consumers,
    sensors: props.sensors,
    batteryApi: props.batteryApi,
    thermal: props.thermal,
    ageSeconds: props.ageSeconds,
  });

  return (
    <section id="tab-overview" className="tabview active" hidden={props.hidden}>
      <Tiles
        current={props.current}
        history={props.history}
        sensors={props.sensors}
        thermal={props.thermal}
        batteryApi={props.batteryApi}
        range={props.range}
        live={props.live}
        focused={props.focused}
        onFocus={props.onFocus}
        tiles={props.tiles}
      />
      <section className="panels">
        <AppsPanel rows={props.apps.slice(0, 6)} onOpenApps={props.onOpenApps} />
        <StatusPanel rows={status} />
      </section>
      <div className="footnote">
        drag a chart to zoom · double-click resets · hover any value for its graph · tap ? to see what a
        number means
      </div>
    </section>
  );
}
