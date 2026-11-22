// The gear's panel: which cards the grid shows (remembered like the original's
// syswatch-tile-config) and which collector the page should ask.
import { useState } from "react";
import { TILE_META } from "../lib/derive";
import type { TileId } from "../lib/derive";

interface Props {
  open: boolean;
  tiles: TileId[];
  onToggleTile: (id: TileId) => void;
  onResetTiles: () => void;
  mode: "sim" | "live";
  apiBase: string;
  onConnect: (base: string) => void;
}

const ORDER = Object.keys(TILE_META) as TileId[];

export function SettingsMenu({ open, tiles, onToggleTile, onResetTiles, mode, apiBase, onConnect }: Props) {
  const [url, setUrl] = useState(apiBase);
  const [notice, setNotice] = useState<string | null>(null);
  if (!open) return null;

  const apply = () => {
    onConnect(url);
    setNotice("asking " + (url.replace(/^https?:\/\//, "") || "the local agent") + " …");
    window.setTimeout(() => setNotice(null), 2600);
  };

  return (
    <div className="settings-menu" data-settings-menu="">
      <div className="sm-section">
        <div className="dm-head">CARDS</div>
        <div className="sm-cards">
          {ORDER.map((id) => {
            const on = tiles.includes(id);
            return (
              <button
                key={id}
                className={on ? "sm-card on" : "sm-card"}
                aria-pressed={on}
                onClick={() => onToggleTile(id)}
              >
                <span className="sm-check">{on ? "✓" : ""}</span>
                <span>{TILE_META[id].label}</span>
              </button>
            );
          })}
        </div>
        <button className="linkbtn sm-reset" onClick={onResetTiles}>
          show all cards
        </button>
      </div>
      <div className="dm-divider"></div>
      <div className="sm-section">
        <div className="dm-head">COLLECTOR</div>
        <div className="sm-mode">
          <span className={mode === "live" ? "livedot" : "simdot"}></span>
          {mode === "live" ? "answering at " + apiBase.replace(/^https?:\/\//, "") : "no agent answering, sampling in the page"}
        </div>
        <div className="sm-connect">
          <input
            className="sm-input"
            value={url}
            spellCheck={false}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") apply();
            }}
            aria-label="Collector address"
          />
          <button className="sm-btn" onClick={apply}>
            Connect
          </button>
        </div>
        <div className="sm-note">
          {notice ?? "The page asks /api/current once a second, and history, consumers and analytics for the range you pick."}
        </div>
      </div>
    </div>
  );
}
