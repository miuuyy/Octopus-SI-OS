import { useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { animate } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { OcIconButton } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { FixtureBrowserDocument, browserPageFromSurface } from "./RunEnvironmentBrowserSurfaces";
import {
  LiveSurfaceEmpty,
  ManualRoomControlWarning,
  isViewerUrl,
} from "./RunEnvironmentSurfacePrimitives";

type SiteTestPreset =
  | "responsive"
  | "4k"
  | "laptop-l"
  | "laptop"
  | "surface-pro-7"
  | "ipad-air"
  | "ipad-mini"
  | "surface-duo"
  | "iphone-15-pro-max"
  | "pixel-8"
  | "iphone-15-pro"
  | "samsung-galaxy-s24-ultra"
  | "iphone-se";

type SiteTestResizeEdge = "left" | "right" | "bottom" | "bottom-left" | "bottom-right";

type SiteTestResizeDrag = {
  edge: SiteTestResizeEdge;
  fitHeight: number;
  fitWidth: number;
  scale: number;
  startHeight: number;
  startPointerX: number;
  startPointerY: number;
  startWidth: number;
};

const SITE_TEST_PRESETS: Array<{ id: SiteTestPreset; label: string; width: number; height: number }> = [
  { id: "responsive", label: "Responsive", width: 390, height: 844 },
  { id: "4k", label: "4K", width: 2560, height: 1440 },
  { id: "laptop-l", label: "Laptop L", width: 1440, height: 900 },
  { id: "laptop", label: "Laptop", width: 1024, height: 768 },
  { id: "surface-pro-7", label: "Surface Pro 7", width: 912, height: 1368 },
  { id: "ipad-air", label: "iPad Air", width: 820, height: 1180 },
  { id: "ipad-mini", label: "iPad Mini", width: 768, height: 1024 },
  { id: "surface-duo", label: "Surface Duo", width: 540, height: 720 },
  { id: "iphone-15-pro-max", label: "iPhone 15 Pro Max", width: 430, height: 932 },
  { id: "pixel-8", label: "Pixel 8", width: 412, height: 915 },
  { id: "iphone-15-pro", label: "iPhone 15 Pro", width: 393, height: 852 },
  { id: "samsung-galaxy-s24-ultra", label: "Samsung Galaxy S24 Ultra", width: 384, height: 824 },
  { id: "iphone-se", label: "iPhone SE", width: 375, height: 667 },
];

export function SiteTestSurface({ environment, surface }: { environment: WorkerEnvironmentProjection; surface?: WorkerEnvironmentSurfaceProjection }) {
  const [presetId, setPresetId] = useState<SiteTestPreset>("responsive");
  const [viewportWidth, setViewportWidth] = useState(390);
  const [viewportHeight, setViewportHeight] = useState(844);
  const [widthDraft, setWidthDraft] = useState<string | null>(null);
  const [heightDraft, setHeightDraft] = useState<string | null>(null);
  const [zoomPercent, setZoomPercent] = useState(100);
  const [resizing, setResizing] = useState(false);
  const [manualControlEnabled, setManualControlEnabled] = useState(false);
  const [manualControlWarningOpen, setManualControlWarningOpen] = useState(false);
  const reduceMotion = useShouldReduceMotion();
  const rotateButtonRef = useRef<HTMLButtonElement>(null);
  const rotateAnimationRef = useRef<{ stop: () => void } | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);
  const resizeDragRef = useRef<SiteTestResizeDrag | null>(null);
  const resizePointRef = useRef<{ x: number; y: number } | null>(null);
  const resizeFrameRef = useRef<number | null>(null);
  const viewportStyle = {
    "--we-site-test-width": `${viewportWidth}px`,
    "--we-site-test-ratio": viewportWidth / viewportHeight,
  } as CSSProperties;
  const pageScale = zoomPercent / 100;
  const pageStyle = {
    width: `${100 / pageScale}%`,
    height: `${100 / pageScale}%`,
    transform: `scale(${pageScale})`,
  } as CSSProperties;

  useEffect(() => () => {
    if (resizeFrameRef.current != null) window.cancelAnimationFrame(resizeFrameRef.current);
    rotateAnimationRef.current?.stop();
  }, []);

  useEffect(() => {
    setManualControlEnabled(false);
    setManualControlWarningOpen(false);
  }, [environment.id, surface?.id, surface?.viewerUrl]);

  function selectPreset(nextPresetId: SiteTestPreset) {
    const preset = SITE_TEST_PRESETS.find((candidate) => candidate.id === nextPresetId);
    if (!preset) return;
    setPresetId(nextPresetId);
    if (nextPresetId !== "responsive") {
      setViewportWidth(preset.width);
      setViewportHeight(preset.height);
    }
    setZoomPercent(100);
    setWidthDraft(null);
    setHeightDraft(null);
  }

  function updateDimension(value: string, dimension: "width" | "height") {
    if (dimension === "width") setWidthDraft(value);
    else setHeightDraft(value);
    const parsed = Number(value);
    const minimum = dimension === "width" ? 240 : 160;
    if (!Number.isFinite(parsed) || parsed < minimum || parsed > 4096) return;
    setPresetId("responsive");
    if (dimension === "width") setViewportWidth(parsed);
    else setViewportHeight(parsed);
  }

  function finishDimension(dimension: "width" | "height") {
    const draft = dimension === "width" ? widthDraft : heightDraft;
    const current = dimension === "width" ? viewportWidth : viewportHeight;
    const minimum = dimension === "width" ? 240 : 160;
    const parsed = Number(draft);
    const next = Number.isFinite(parsed) ? Math.min(4096, Math.max(minimum, parsed)) : current;
    if (dimension === "width") {
      setViewportWidth(next);
      setWidthDraft(null);
    } else {
      setViewportHeight(next);
      setHeightDraft(null);
    }
  }

  function applyResizePoint() {
    resizeFrameRef.current = null;
    const drag = resizeDragRef.current;
    const point = resizePointRef.current;
    if (!drag || !point) return;
    resizePointRef.current = null;
    const changesHeight = drag.edge === "bottom" || drag.edge === "bottom-left" || drag.edge === "bottom-right";
    const changesWidth = drag.edge !== "bottom";
    const direction = drag.edge === "left" || drag.edge === "bottom-left" ? -1 : 1;
    if (changesWidth) {
      const rawWidth = drag.startWidth + ((point.x - drag.startPointerX) * direction * 2) / drag.scale;
      setViewportWidth(clampSiteTestDimension(rawWidth, 240, drag.fitWidth / drag.scale));
    }
    if (changesHeight) {
      const rawHeight = drag.startHeight + (point.y - drag.startPointerY) / drag.scale;
      setViewportHeight(clampSiteTestDimension(rawHeight, 160, drag.fitHeight / drag.scale));
    }
  }

  function beginResize(event: ReactPointerEvent<HTMLButtonElement>, edge: SiteTestResizeEdge) {
    const viewport = viewportRef.current;
    const body = bodyRef.current;
    if (!viewport || !body || presetId !== "responsive") return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    const viewportBounds = viewport.getBoundingClientRect();
    const bodyBounds = body.getBoundingClientRect();
    const scale = viewportWidth > 0 ? viewportBounds.width / viewportWidth : 1;
    resizeDragRef.current = {
      edge,
      fitHeight: Math.max(160 * scale, bodyBounds.height - 40),
      fitWidth: Math.max(240 * scale, bodyBounds.width - 40),
      scale: scale > 0 ? scale : 1,
      startHeight: viewportHeight,
      startPointerX: event.clientX,
      startPointerY: event.clientY,
      startWidth: viewportWidth,
    };
    resizePointRef.current = null;
    setWidthDraft(null);
    setHeightDraft(null);
    setResizing(true);
  }

  function moveResize(event: ReactPointerEvent<HTMLButtonElement>) {
    if (!resizeDragRef.current) return;
    resizePointRef.current = { x: event.clientX, y: event.clientY };
    resizeFrameRef.current ??= window.requestAnimationFrame(applyResizePoint);
  }

  function finishResize() {
    if (resizeFrameRef.current != null) {
      window.cancelAnimationFrame(resizeFrameRef.current);
      applyResizePoint();
    }
    resizeDragRef.current = null;
    resizePointRef.current = null;
    setResizing(false);
  }

  return (
    <div className="we-site-test-shell">
      <div className="we-site-test-toolbar">
        <label className="we-site-test-dimensions-label" htmlFor="site-test-device-preset">Dimensions:</label>
        <select
          aria-label="Viewport preset"
          className="we-site-test-preset"
          id="site-test-device-preset"
          onChange={(event) => {
            selectPreset(event.currentTarget.value as SiteTestPreset);
            event.currentTarget.blur();
          }}
          value={presetId}
        >
          {SITE_TEST_PRESETS.map((preset) => <option key={preset.id} value={preset.id}>{preset.label}</option>)}
        </select>
        <div className="we-site-test-dimensions">
          <input
            aria-label="Viewport width"
            max={4096}
            min={240}
            onBlur={() => finishDimension("width")}
            onChange={(event) => updateDimension(event.currentTarget.value, "width")}
            onFocus={() => setWidthDraft(String(viewportWidth))}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
            }}
            type="number"
            value={widthDraft ?? viewportWidth}
          />
          <span>×</span>
          <input
            aria-label="Viewport height"
            max={4096}
            min={160}
            onBlur={() => finishDimension("height")}
            onChange={(event) => updateDimension(event.currentTarget.value, "height")}
            onFocus={() => setHeightDraft(String(viewportHeight))}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
            }}
            type="number"
            value={heightDraft ?? viewportHeight}
          />
        </div>
        <OcIconButton
          buttonRef={rotateButtonRef}
          glyph="rotateDevice"
          label="Rotate viewport"
          onClick={(event) => {
            setViewportWidth(viewportHeight);
            setViewportHeight(viewportWidth);
            setPresetId("responsive");
            if (!reduceMotion) {
              const svg = rotateButtonRef.current?.querySelector("svg");
              if (svg) {
                rotateAnimationRef.current?.stop();
                svg.style.transformOrigin = "50% 50%";
                rotateAnimationRef.current = animate(
                  svg,
                  {
                    rotate: [0, 44, 108, 158, 176, 180],
                    scale: [1, 0.96, 0.98, 1.06, 1.02, 1],
                  },
                  {
                    duration: 0.36,
                    ease: "easeOut",
                    times: [0, 0.22, 0.42, 0.64, 0.84, 1],
                  },
                );
              }
            }
            event.currentTarget.blur();
          }}
          size="sm"
          variant="ghost"
        />
        <select
          aria-label="Browser zoom"
          className="we-site-test-zoom"
          onChange={(event) => {
            setZoomPercent(Number(event.currentTarget.value));
            event.currentTarget.blur();
          }}
          value={zoomPercent}
        >
          {[50, 75, 100, 125, 150, 200].map((value) => <option key={value} value={value}>{value}%</option>)}
        </select>
      </div>
      <div className="we-site-test-body" ref={bodyRef}>
        <div className={`we-site-test-stage${resizing ? " is-resizing" : ""}`} style={viewportStyle}>
          <div className="we-site-test-viewport" ref={viewportRef}>
            <div className="we-site-test-page" style={pageStyle}>
              {surface?.mode === "fixture" || surface?.mode === "evidence" ? (
                <FixtureBrowserDocument page={browserPageFromSurface(surface)} />
              ) : surface?.mode === "direct" && isViewerUrl(surface.viewerUrl) ? (
                <iframe
                  referrerPolicy="no-referrer"
                  sandbox="allow-forms allow-modals allow-popups allow-same-origin allow-scripts"
                  src={surface.viewerUrl}
                  style={{ pointerEvents: manualControlEnabled && !surface.readOnly ? "auto" : "none" }}
                  tabIndex={manualControlEnabled && !surface.readOnly ? 0 : -1}
                  title={surface.label}
                />
              ) : (
                <LiveSurfaceEmpty
                  detail={environment.session
                    ? "The room has a browser session, but its preview descriptor carries no supported viewer URL."
                    : "A live preview appears only when the Worker Environment projects an explicit preview surface descriptor."}
                  glyph="sites"
                  title={environment.session ? "Viewer endpoint unavailable" : "No live site preview"}
                />
              )}
            </div>
          </div>
          {presetId === "responsive" ? (
            <div className="we-site-test-resize-handles">
              <SiteTestEdgeHandle edge="left" label="Resize device viewport from the left edge" onPointerDown={beginResize} onPointerMove={moveResize} onPointerUp={finishResize} />
              <SiteTestEdgeHandle edge="right" label="Resize device viewport from the right edge" onPointerDown={beginResize} onPointerMove={moveResize} onPointerUp={finishResize} />
              <SiteTestEdgeHandle edge="bottom" label="Resize device viewport from the bottom edge" onPointerDown={beginResize} onPointerMove={moveResize} onPointerUp={finishResize} />
              <SiteTestCornerHandle edge="bottom-left" label="Resize device viewport from the bottom-left corner" onPointerDown={beginResize} onPointerMove={moveResize} onPointerUp={finishResize} />
              <SiteTestCornerHandle edge="bottom-right" label="Resize device viewport from the bottom-right corner" onPointerDown={beginResize} onPointerMove={moveResize} onPointerUp={finishResize} />
            </div>
          ) : null}
          {surface?.mode === "direct" && isViewerUrl(surface.viewerUrl) && !surface.readOnly && !manualControlEnabled ? (
            <button
              aria-label="Enable manual site preview control"
              className="we-browser-manual-gate"
              onClick={() => setManualControlWarningOpen(true)}
              type="button"
            >
              <span><Glyph name="sites" size={14} />Enable manual control</span>
            </button>
          ) : null}
        </div>
      </div>
      <ManualRoomControlWarning
        glyph="sites"
        onCancel={() => setManualControlWarningOpen(false)}
        onContinue={() => {
          setManualControlEnabled(true);
          setManualControlWarningOpen(false);
        }}
        open={manualControlWarningOpen}
        title="Manual site preview control"
      />
    </div>
  );
}

type SiteTestResizeHandleProps = {
  edge: SiteTestResizeEdge;
  label: string;
  onPointerDown: (event: ReactPointerEvent<HTMLButtonElement>, edge: SiteTestResizeEdge) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLButtonElement>) => void;
  onPointerUp: () => void;
};

function SiteTestEdgeHandle({ edge, label, onPointerDown, onPointerMove, onPointerUp }: SiteTestResizeHandleProps) {
  return (
    <button
      aria-label={label}
      className={`we-site-test-resize-handle is-${edge}`}
      onLostPointerCapture={onPointerUp}
      onPointerCancel={onPointerUp}
      onPointerDown={(event) => onPointerDown(event, edge)}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      tabIndex={-1}
      type="button"
    >
      <span><i /><i /></span>
    </button>
  );
}

function SiteTestCornerHandle({ edge, label, onPointerDown, onPointerMove, onPointerUp }: SiteTestResizeHandleProps) {
  return (
    <button
      aria-label={label}
      className={`we-site-test-resize-corner is-${edge}`}
      onLostPointerCapture={onPointerUp}
      onPointerCancel={onPointerUp}
      onPointerDown={(event) => onPointerDown(event, edge)}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      tabIndex={-1}
      type="button"
    >
      <svg aria-hidden="true" className={edge === "bottom-left" ? "is-mirrored" : ""} fill="none" viewBox="0 0 20 20">
        <path d="M6 11.75L11.75 6" />
        <path d="M7 15.5L15.5 7" />
      </svg>
    </button>
  );
}

function clampSiteTestDimension(value: number, minimum: number, fitMaximum: number): number {
  const maximum = Math.min(4096, Math.max(minimum, fitMaximum));
  return Math.min(maximum, Math.max(minimum, Math.round(value)));
}
