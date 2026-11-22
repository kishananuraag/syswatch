// The device switcher. One machine today; the same panel is where a paired
// phone, laptop or TV will appear once the relay hands out pairing codes.
import { useState } from "react";

interface Props {
  open: boolean;
  pairHint: boolean;
  onAdd: () => void;
  mode: "sim" | "live";
  apiBase: string;
  onConnect: (base: string) => void;
}

export function DeviceMenu({ open, pairHint, onAdd, mode, apiBase }: Props) {
  const [copied, setCopied] = useState(false);
  if (!open) return null;
  const code = "482 913";

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div id="deviceMenu" className="device-menu" data-device-menu="">
      <div className="dm-head">YOUR DEVICES</div>
      <button className="dm-row selected" data-device="thismac">
        <span className="dot"></span>
        <span className="dm-main">
          <span className="dm-name">This Mac</span>
          <span className="dm-sub">You are here</span>
        </span>
        <svg className="check" viewBox="0 0 16 16" width="14" height="14" aria-label="Viewing now">
          <path
            d="M3 8.5l3 3 7-7"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
      <div className="dm-divider"></div>
      <button className="dm-add" id="addDevice" onClick={onAdd}>
        + Add a device
      </button>
      {pairHint && (
        <div id="pairHint" className="pair-hint">
          <div>Pairing needs the relay pairing code (see crates/relay).</div>
          <div className="pair-code">
            <code>{code}</code>
            <button className="linkbtn" onClick={copy}>
              {copied ? "copied" : "copy"}
            </button>
          </div>
          <div className="pair-note">Enter it on the phone or TV within 5 minutes.</div>
        </div>
      )}
      <div className="dm-divider"></div>
      <div className="dm-foot">
        <span className="dm-sub">Collector</span>
        <span className={mode === "live" ? "dm-mode live" : "dm-mode"}>
          <span className={mode === "live" ? "livedot" : "simdot"}></span>
          {mode === "live" ? `live · ${apiBase.replace(/^https?:\/\//, "")}` : "built-in sampler"}
        </span>
      </div>
    </div>
  );
}
