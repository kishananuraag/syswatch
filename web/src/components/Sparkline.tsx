// Sparklines: one teal line per tile, drawn in a 100×30 box stretched to the
// card's width so a narrow phone card keeps the same shape as a wide one.
import { areaPath, seriesBounds, sparkPath } from "../lib/derive";

export function Spark({ values, height = 44 }: { values: number[]; height?: number }) {
  const path = sparkPath(values, null);
  return (
    <svg
      className="spark"
      viewBox="0 0 100 30"
      preserveAspectRatio="none"
      style={{ width: "100%", height }}
      aria-hidden="true"
    >
      <path
        d={path}
        fill="none"
        stroke="#6EE7C8"
        strokeWidth={1.5}
        vectorEffect="non-scaling-stroke"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function WideSpark({ first, second }: { first: number[]; second: number[] }) {
  const bounds = seriesBounds([first, second]);
  return (
    <svg
      className="spark"
      viewBox="0 0 100 30"
      preserveAspectRatio="none"
      style={{ width: "100%", height: 120 }}
      aria-hidden="true"
    >
      <path
        d="M0 7.5H100 M0 15H100 M0 22.5H100"
        fill="none"
        stroke="#1E272E"
        strokeWidth={1}
        vectorEffect="non-scaling-stroke"
      />
      <path d={areaPath(first, bounds)} fill="rgba(110,231,200,0.09)" stroke="none" />
      <path
        d={sparkPath(second, bounds)}
        fill="none"
        stroke="#8A97A3"
        strokeWidth={1.4}
        vectorEffect="non-scaling-stroke"
        strokeLinejoin="round"
      />
      <path
        d={sparkPath(first, bounds)}
        fill="none"
        stroke="#6EE7C8"
        strokeWidth={1.5}
        vectorEffect="non-scaling-stroke"
        strokeLinejoin="round"
      />
    </svg>
  );
}
