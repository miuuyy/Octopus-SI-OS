import { ClientSurfaceVideo } from "./ClientSurfaceVideo";
import { motion } from "motion/react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent, RefObject } from "react";
import { useAppearance, type AppearanceSettings } from "../appearance";
import { formatProjectionError, hasProjectionBridge, readProjection } from "../projectionClient";
import { useProjectionGeneration } from "../projectionLive";
import { readPath } from "../runtimeFormat";
import { readClientExtensionFilesystemSource } from "../extensions/transport";
import { clearClientSurfaceRuntimeIssue, reportClientSurfaceRuntimeIssue } from "../extensions/runtimeIssues";
import { Glyph } from "../components/IconGlyphs";
import { OCTOPUS_FLOATING_SURFACE_CLASS, OCTOPUS_WIDGET_SURFACE_CLASS } from "../design-system/components";
import type { OctopusClientAccessProfile } from "../electron";
import { CLIENT_SURFACE_ACTION_EVENT } from "../clientEvents";
import type {
  AdmittedClientSurfaceDocument,
  ClientSurfaceAction,
  ClientSurfaceActionEvent,
  ClientSurfaceBinding,
  ClientSurfaceDocument,
  ClientSurfaceHostSource,
  ClientSurfaceNode,
  ClientSurfaceNumberListValue,
  ClientSurfaceNumberValue,
  ClientSurfaceTone,
  ClientSurfaceValue,
  VoiceWidgetLayout,
} from "./types";
import {
  DEFAULT_CLIENT_SURFACE_COLLECTION_LIMIT,
  MAX_CLIENT_SURFACE_COLLECTION_LIMIT,
  isSafeClientSurfaceImageSource,
} from "./sourceContract";
import {
  subscribeVoiceSpatialInteraction,
  type VoiceSpatialCorner,
  type VoiceSpatialInteraction,
} from "./voiceSpatialControl";
import "./client-surfaces.css";
import { homeSurfaceLayoutIdentity } from "./homePresentation";
import { HOME_SURFACE_FORMING, HOME_SURFACE_LEAVING } from "./homeSurfaceMotion";
import { placeVoiceWidget, type WidgetRect } from "./voiceWidgetPlacement";
import scrollbarCss from "../design-system/scrollbars.css?raw";

const VOICE_WIDGET_LAYOUT_RESET_EVENT = "octopus:voice-widget-layout-reset";

type Props = {
  accessProfile: OctopusClientAccessProfile;
  admitted: AdmittedClientSurfaceDocument;
  onAction?: (action: ClientSurfaceActionEvent) => boolean | void;
  onDismiss?: () => void;
  voicePlacement?: VoiceSurfacePlacement;
  voiceForming?: boolean;
  voiceLeaving?: boolean;
};

export type VoiceSurfacePlacement = {
  count: number;
  index: number;
  slot: "left" | "right";
};

type DataState = {
  loading: boolean;
  sources: Record<string, unknown>;
  error: string;
};

const EMPTY_PROJECTION_SOURCES: NonNullable<ClientSurfaceDocument["sources"]> = [];
const EMPTY_HOST_SOURCES: ClientSurfaceHostSource[] = [];

export function ClientSurfaceRenderer({ accessProfile, admitted, onAction, onDismiss, voiceForming = false, voiceLeaving = false, voicePlacement }: Props) {
  const appearance = useAppearance();
  const document = admitted.document;
  const densityClass = document.render.kind === "native" ? ` density-${document.render.density ?? "standard"}` : "";
  const dataState = useSurfaceData(admitted);
  useEffect(() => {
    if (dataState.loading) return;
    const scope = { extensionId: admitted.sourceExtensionId, documentId: document.id, sourceDigest: admitted.sourceDigest };
    if (dataState.error) reportClientSurfaceRuntimeIssue(scope, "source", dataState.error);
    else clearClientSurfaceRuntimeIssue(scope, "source");
  }, [admitted.sourceDigest, admitted.sourceExtensionId, dataState.error, dataState.loading, document.id]);
  const voiceWidgetLayout = document.target.kind === "voice.widget" ? document.target.layout : undefined;
  const widgetResize = useVoiceWidgetResize(admitted.sourceArtifactId ? homeSurfaceLayoutIdentity(admitted) : document.id, accessProfile, voicePlacement, voiceWidgetLayout);
  const widgetContentRef = useVoiceWidgetContentFit(
    widgetResize.ref,
    voiceWidgetLayout?.content_fit === "scale",
    `${document.id}:${document.revision}`,
  );
  const context = useMemo(() => ({
    data: document.data ?? {},
    sources: dataState.sources,
  }), [dataState.sources, document.data]);
  const handleAction = useCallback((event: ClientSurfaceActionEvent) => {
    const detail: ClientSurfaceActionEvent = {
      ...event,
      documentRevision: document.revision,
      source: {
        digest: admitted.sourceDigest,
        artifactId: admitted.sourceArtifactId,
        runId: admitted.sourceRunId,
        interfaceId: admitted.sourceInterfaceId,
        sequence: admitted.sourceSequence,
      },
    };
    if (onAction?.(detail) !== true) window.dispatchEvent(new CustomEvent(CLIENT_SURFACE_ACTION_EVENT, { detail }));
    return true;
  }, [admitted, document.revision, onAction]);
  const body = dataState.loading ? (
    <ClientSurfaceState title="Reading source" detail="Waiting for declared client data" />
  ) : dataState.error ? (
    <ClientSurfaceState title="Source unavailable" detail={dataState.error} />
  ) : document.render.kind === "native" ? (
    <NativeNode
      context={context}
      documentId={document.id}
      node={document.render.root}
      onAction={handleAction}
      path="root"
    />
  ) : (
    <SandboxSurface admitted={admitted} appearance={appearance} context={context} onAction={handleAction} />
  );

  if (document.target.kind === "voice.widget") {
    return (
      <motion.section
        animate={{ opacity: voiceLeaving ? 0 : 1 }}
        className={`voice-projection-card client-surface-frame ${OCTOPUS_WIDGET_SURFACE_CLASS} is-${document.render.kind}${densityClass}${widgetResize.layoutReady ? " is-layout-ready" : ""}${widgetResize.interacting ? " is-interacting" : ""}`}
        data-content-fit={document.target.layout.content_fit}
        data-gesture-label={document.title}
        data-surface-document={document.id}
        data-surface-artifact={admitted.sourceArtifactId}
        data-surface-run={admitted.sourceRunId}
        data-surface-interface={admitted.sourceInterfaceId}
        data-voice-forming={(voiceForming && !voiceLeaving) || undefined}
        data-voice-leaving={voiceLeaving || undefined}
        data-voice-slot={voicePlacement?.slot}
        inert={voiceLeaving || undefined}
        initial={{ opacity: 0 }}
        ref={widgetResize.ref}
        style={widgetResize.style}
        transition={voiceLeaving ? HOME_SURFACE_LEAVING : voiceForming ? HOME_SURFACE_FORMING : { duration: 0.16, ease: [0.19, 1, 0.22, 1] }}
      >
        <button
          aria-label={`Move ${document.title}`}
          className="voice-widget-drag-handle"
          data-gesture-ignore=""
          onLostPointerCapture={widgetResize.onPointerEnd}
          onPointerCancel={widgetResize.onPointerEnd}
          onPointerDown={widgetResize.onDragPointerDown}
          onPointerMove={widgetResize.onPointerMove}
          onPointerUp={widgetResize.onPointerEnd}
          type="button"
        />
        <div className="voice-widget-content" ref={widgetContentRef}>
          <header>
            <span data-header-tone={document.header?.tone ?? "default"}>
              {document.header?.show_glyph ? <Glyph name={document.glyph} size={14} /> : null}
              <strong>{document.title}</strong>
            </span>
            {onDismiss ? (
              <button aria-label={`Hide ${document.title}`} className="client-surface-dismiss" data-surface-dismiss="" onClick={onDismiss} type="button">
                <Glyph name="close" size={14} />
              </button>
            ) : null}
          </header>
          {body}
        </div>
        {widgetResize.corners.map((corner) => (
          <span
            aria-hidden="true"
            className={`voice-widget-resize-handle is-${corner}`}
            key={corner}
            onLostPointerCapture={widgetResize.onPointerEnd}
            onPointerCancel={widgetResize.onPointerEnd}
            onPointerDown={(event) => widgetResize.onResizePointerDown(corner, event)}
            onPointerMove={widgetResize.onPointerMove}
            onPointerUp={widgetResize.onPointerEnd}
          />
        ))}
      </motion.section>
    );
  }

  if (document.target.kind === "voice.window") {
    return (
      <motion.section
        animate={voiceLeaving ? { opacity: 0, scale: 0.985, y: 5 } : { opacity: 1, scale: 1, y: 0 }}
        className={`voice-surface-window client-surface-frame ${OCTOPUS_FLOATING_SURFACE_CLASS} is-${document.render.kind}${densityClass}${widgetResize.layoutReady ? " is-layout-ready" : ""}${widgetResize.interacting ? " is-interacting" : ""}`}
        data-gesture-label={document.title}
        data-surface-document={document.id}
        data-surface-artifact={admitted.sourceArtifactId}
        data-surface-run={admitted.sourceRunId}
        data-surface-interface={admitted.sourceInterfaceId}
        data-voice-forming={(voiceForming && !voiceLeaving) || undefined}
        data-voice-leaving={voiceLeaving || undefined}
        data-voice-slot={voicePlacement?.slot}
        data-window-size={document.target.size ?? "standard"}
        inert={voiceLeaving || undefined}
        initial={{ opacity: 0, scale: 0.985, y: 5 }}
        ref={widgetResize.ref}
        style={widgetResize.style}
        transition={voiceLeaving ? HOME_SURFACE_LEAVING : voiceForming ? HOME_SURFACE_FORMING : { duration: 0.2, ease: [0.19, 1, 0.22, 1] }}
      >
        <button
          aria-label={`Move ${document.title}`}
          className="voice-widget-drag-handle"
          data-gesture-ignore=""
          onLostPointerCapture={widgetResize.onPointerEnd}
          onPointerCancel={widgetResize.onPointerEnd}
          onPointerDown={widgetResize.onDragPointerDown}
          onPointerMove={widgetResize.onPointerMove}
          onPointerUp={widgetResize.onPointerEnd}
          type="button"
        />
        <header>
          <span><strong>{document.title}</strong></span>
          {onDismiss ? (
            <button aria-label={`Hide ${document.title}`} data-surface-dismiss="" onClick={onDismiss} type="button">
              <Glyph className="icon-xs" name="close" size={16} />
            </button>
          ) : null}
        </header>
        <div className="voice-surface-window__body">{body}</div>
        {widgetResize.corners.map((corner) => (
          <span
            aria-hidden="true"
            className={`voice-widget-resize-handle is-${corner}`}
            key={corner}
            onLostPointerCapture={widgetResize.onPointerEnd}
            onPointerCancel={widgetResize.onPointerEnd}
            onPointerDown={(event) => widgetResize.onResizePointerDown(corner, event)}
            onPointerMove={widgetResize.onPointerMove}
            onPointerUp={widgetResize.onPointerEnd}
          />
        ))}
      </motion.section>
    );
  }

  return (
    <section className={`client-surface-page is-${document.render.kind}${densityClass}`} data-surface-document={document.id}>
      {body}
    </section>
  );
}

type WidgetGeometry = {
  height: number | null;
  stageHeight: number | null;
  stageWidth: number | null;
  width: number | null;
  x: number | null;
  y: number | null;
  z: number;
  slot: VoiceSurfacePlacement["slot"] | null;
};

type WidgetInteractionStart = {
  bounds: DOMRect;
  corner?: VoiceSpatialCorner;
  inputId: string;
  kind: "drag" | "resize" | "transform";
  pointerX: number;
  pointerY: number;
  stageBounds: DOMRect;
  x: number;
  y: number;
  z: number;
};

type WidgetSizePolicy = {
  minHeight: number;
  minWidth: number;
  preferredHeight: number | null;
  preferredWidth: number | null;
  maxHeight: number | null;
  maxWidth: number | null;
};

type WidgetSizeBounds = {
  minHeight: number;
  minWidth: number;
  preferredHeight: number;
  preferredWidth: number;
  maxHeight: number;
  maxWidth: number;
};

const VOICE_WIDGET_RESIZE_CORNERS: VoiceSpatialCorner[] = ["north-west", "north-east", "south-east", "south-west"];
const VOICE_WIDGET_LAYOUT_KEY = "octopus.voice.widget-layout.v3";
const LAB_VOICE_WIDGET_LAYOUT_KEY = "octopus.voice.widget-layout.v4.lab";
const DEFAULT_WIDGET_GEOMETRY: WidgetGeometry = {
  height: null,
  stageHeight: null,
  stageWidth: null,
  width: null,
  x: null,
  y: null,
  z: 1,
  slot: null,
};
const widgetStageGeometries = new WeakMap<HTMLElement, Map<HTMLElement, WidgetGeometry>>();
const WIDGET_EDGE_GAP = 8;
const WIDGET_MIN_HEIGHT = 96;
const WIDGET_MIN_WIDTH = 150;
let voiceWidgetZ = 1;

function useVoiceWidgetContentFit(
  containerRef: RefObject<HTMLElement | null>,
  enabled: boolean,
  contentKey: string,
) {
  const contentRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const container = containerRef.current;
    const content = contentRef.current;
    if (!content) return;
    if (!enabled || !container) {
      content.style.removeProperty("--voice-widget-content-zoom");
      delete content.dataset.contentScale;
      return;
    }
    let frame = 0;

    const measure = () => {
      frame = 0;
      const availableWidth = container.clientWidth;
      const availableHeight = container.clientHeight;
      if (availableWidth <= 0 || availableHeight <= 0) return;

      let lower = 0.25;
      let upper = 1;
      for (let index = 0; index < 10; index += 1) {
        const candidate = (lower + upper) / 2;
        content.style.setProperty("--voice-widget-content-zoom", String(candidate));
        const bounds = content.getBoundingClientRect();
        if (bounds.width <= availableWidth + 0.25 && bounds.height <= availableHeight + 0.25) lower = candidate;
        else upper = candidate;
      }

      const next = Math.floor(lower * 10_000) / 10_000;
      content.style.setProperty("--voice-widget-content-zoom", String(next));
      content.dataset.contentScale = String(next);
    };

    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    const resizeObserver = new ResizeObserver(() => schedule());
    const mutationObserver = new MutationObserver(schedule);
    resizeObserver.observe(container);
    mutationObserver.observe(content, { childList: true, characterData: true, subtree: true });
    measure();
    void document.fonts?.ready.then(schedule);

    return () => {
      cancelAnimationFrame(frame);
      resizeObserver.disconnect();
      mutationObserver.disconnect();
    };
  }, [containerRef, contentKey, enabled]);

  return contentRef;
}

export function resetVoiceWidgetLayout(accessProfile: OctopusClientAccessProfile): string | null {
  const storageKey = voiceWidgetLayoutStorageKey(accessProfile);
  const previous = window.localStorage.getItem(storageKey);
  window.localStorage.removeItem(storageKey);
  if (window.localStorage.getItem(storageKey) !== null) throw new Error("Voice widget positions could not be reset.");
  window.dispatchEvent(new CustomEvent(VOICE_WIDGET_LAYOUT_RESET_EVENT, { detail: { accessProfile } }));
  return previous;
}

export function restoreVoiceWidgetLayout(accessProfile: OctopusClientAccessProfile, snapshot: string | null): void {
  const storageKey = voiceWidgetLayoutStorageKey(accessProfile);
  if (snapshot === null) window.localStorage.removeItem(storageKey);
  else window.localStorage.setItem(storageKey, snapshot);
  if (window.localStorage.getItem(storageKey) !== snapshot) throw new Error("Voice widget positions could not be restored.");
  window.dispatchEvent(new CustomEvent(VOICE_WIDGET_LAYOUT_RESET_EVENT, { detail: { accessProfile } }));
}

export function useVoiceWidgetResize(
  documentId: string,
  accessProfile: OctopusClientAccessProfile,
  placement?: VoiceSurfacePlacement,
  layout?: VoiceWidgetLayout,
) {
  const ref = useRef<HTMLElement>(null);
  const active = useRef<WidgetInteractionStart | null>(null);
  const geometryKey = `${accessProfile}:${documentId}`;
  const geometryDocument = useRef(geometryKey);
  const spatialHandler = useRef<(interaction: VoiceSpatialInteraction) => void>(() => undefined);
  const sizePolicy = useMemo(() => widgetSizePolicy(layout), [layout]);
  const [geometry, setGeometry] = useState<WidgetGeometry>(() => readWidgetGeometry(documentId, accessProfile));
  const [interacting, setInteracting] = useState(false);
  const [layoutReady, setLayoutReady] = useState(false);

  useLayoutEffect(() => {
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    if (element && stage && geometryDocument.current === geometryKey && geometry.x != null && geometry.y != null) {
      widgetStageGeometries.get(stage)?.set(element, geometry);
    }
  }, [geometry, geometryKey]);

  useLayoutEffect(() => {
    const changed = geometryDocument.current !== geometryKey;
    const current = changed ? readWidgetGeometry(documentId, accessProfile) : geometry;
    if (changed) {
      geometryDocument.current = geometryKey;
      active.current = null;
      setInteracting(false);
      setLayoutReady(false);
      setGeometry(current);
    }
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    if (!placement || !element || !stage) return;
    const next = initializeWidgetGeometry(element, stage, current, placement, sizePolicy);
    let peers = widgetStageGeometries.get(stage);
    if (!peers) widgetStageGeometries.set(stage, peers = new Map());
    peers.set(element, next);
    setGeometry(next);
  }, [accessProfile, documentId, geometryKey, placement?.count, placement?.index, placement?.slot, sizePolicy]);

  useLayoutEffect(() => {
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    return () => { if (element && stage) widgetStageGeometries.get(stage)?.delete(element); };
  }, [geometryKey]);

  useLayoutEffect(() => {
    if (!placement) return;
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    if (!element || !stage || active.current) return;
    const bounds = widgetSizeBounds(sizePolicy, stage.clientWidth, stage.clientHeight, element);
    const widthOutside = geometry.width != null && (geometry.width < bounds.minWidth || geometry.width > bounds.maxWidth);
    const heightOutside = geometry.height != null && (geometry.height < bounds.minHeight || geometry.height > bounds.maxHeight);
    if (!widthOutside && !heightOutside) return;
    setGeometry((current) => constrainWidgetGeometry(element, stage, current, sizePolicy));
  }, [documentId, geometry.height, geometry.width, placement?.slot, sizePolicy]);

  useEffect(() => {
    if (!placement || geometry.x == null || geometry.y == null || geometryDocument.current !== geometryKey) return;
    if (geometry.stageWidth != null && geometry.stageHeight != null) {
      writeWidgetGeometry(documentId, accessProfile, geometry);
    }
  }, [accessProfile, documentId, geometry, geometryKey, placement?.slot]);

  const positioned = !placement
    || (geometry.x != null && geometry.y != null && geometryDocument.current === geometryKey);

  useEffect(() => {
    if (!positioned) {
      setLayoutReady(false);
      return;
    }
    let firstFrame = 0;
    let secondFrame = 0;
    firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(() => setLayoutReady(true));
    });
    return () => {
      cancelAnimationFrame(firstFrame);
      cancelAnimationFrame(secondFrame);
    };
  }, [geometryKey, positioned]);

  useEffect(() => {
    if (!placement) return;
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    if (!element || !stage) return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (active.current) return;
        const peers = widgetStageGeometries.get(stage);
        const current = peers?.get(element);
        if (!peers || !current) return;
        const next = initializeWidgetGeometry(element, stage, current, placement, sizePolicy);
        peers.set(element, next);
        setGeometry(next);
      });
    });
    observer.observe(stage);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, [documentId, placement?.slot, sizePolicy]);

  function beginInteractionAt(
    kind: WidgetInteractionStart["kind"],
    corner: VoiceSpatialCorner | undefined,
    clientX: number,
    clientY: number,
    inputId: string,
  ): boolean {
    const element = ref.current;
    const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
    if (!element || !stage || !placement || (active.current && active.current.inputId !== inputId)) return false;
    const stageBounds = stage.getBoundingClientRect();
    const rect = element.getBoundingClientRect();
    const bounds = {
      height: element.offsetHeight,
      width: element.offsetWidth,
      x: rect.x,
      y: rect.y,
    } as DOMRect;
    const z = ++voiceWidgetZ;
    active.current = {
      bounds,
      corner,
      inputId,
      kind,
      pointerX: clientX,
      pointerY: clientY,
      stageBounds,
      x: rect.left - stageBounds.left,
      y: rect.top - stageBounds.top,
      z,
    };
    setGeometry((current) => ({ ...current, z }));
    setInteracting(true);
    return true;
  }

  function beginPointerInteraction(
    kind: WidgetInteractionStart["kind"],
    corner: VoiceSpatialCorner | undefined,
    event: ReactPointerEvent<HTMLElement>,
  ) {
    const inputId = `pointer:${event.pointerId}`;
    if (!beginInteractionAt(kind, corner, event.clientX, event.clientY, inputId)) return;
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function onDragPointerDown(event: ReactPointerEvent<HTMLButtonElement>) {
    beginPointerInteraction("drag", undefined, event);
  }

  function onResizePointerDown(corner: VoiceSpatialCorner, event: ReactPointerEvent<HTMLSpanElement>) {
    beginPointerInteraction("resize", corner, event);
  }

  function moveInteractionAt(clientX: number, clientY: number, inputId: string, scale = 1) {
    const start = active.current;
    const element = ref.current;
    if (!start || start.inputId !== inputId || !element) return;
    const deltaX = clientX - start.pointerX;
    const deltaY = clientY - start.pointerY;
    const stageWidth = start.stageBounds.width;
    const stageHeight = start.stageBounds.height;
    if (start.kind === "transform") {
      const bounds = widgetSizeBounds(sizePolicy, stageWidth, stageHeight, element);
      const width = clamp(start.bounds.width * scale, bounds.minWidth, bounds.maxWidth);
      const height = clamp(start.bounds.height * scale, bounds.minHeight, bounds.maxHeight);
      setGeometry((current) => ({
        ...current,
        height,
        stageHeight,
        stageWidth,
        width,
        x: clamp(start.x + (start.bounds.width - width) / 2 + deltaX, WIDGET_EDGE_GAP, Math.max(WIDGET_EDGE_GAP, stageWidth - width - WIDGET_EDGE_GAP)),
        y: clamp(start.y + (start.bounds.height - height) / 2 + deltaY, WIDGET_EDGE_GAP, Math.max(WIDGET_EDGE_GAP, stageHeight - height - WIDGET_EDGE_GAP)),
        z: start.z,
      }));
      return;
    }
    if (start.kind === "drag") {
      setGeometry((current) => ({
        ...current,
        stageHeight,
        stageWidth,
        x: clamp(start.x + deltaX, WIDGET_EDGE_GAP, Math.max(WIDGET_EDGE_GAP, stageWidth - start.bounds.width - WIDGET_EDGE_GAP)),
        y: clamp(start.y + deltaY, WIDGET_EDGE_GAP, Math.max(WIDGET_EDGE_GAP, stageHeight - start.bounds.height - WIDGET_EDGE_GAP)),
      }));
      return;
    }
    const corner = start.corner;
    if (!corner) return;
    const bounds = widgetSizeBounds(sizePolicy, stageWidth, stageHeight, element);
    const next: WidgetGeometry = {
      height: start.bounds.height,
      stageHeight,
      stageWidth,
      width: start.bounds.width,
      x: start.x,
      y: start.y,
      z: start.z,
      slot: geometry.slot,
    };

    if (corner.endsWith("east")) {
      const maxWidth = Math.min(bounds.maxWidth, Math.max(bounds.minWidth, stageWidth - start.x - WIDGET_EDGE_GAP));
      next.width = clamp(start.bounds.width + deltaX, bounds.minWidth, maxWidth);
    }
    if (corner.startsWith("south")) {
      const maxHeight = Math.min(bounds.maxHeight, Math.max(bounds.minHeight, stageHeight - start.y - WIDGET_EDGE_GAP));
      next.height = clamp(start.bounds.height + deltaY, bounds.minHeight, maxHeight);
    }
    if (corner.endsWith("west")) {
      const maxWidth = Math.min(bounds.maxWidth, Math.max(bounds.minWidth, start.x + start.bounds.width - WIDGET_EDGE_GAP));
      const width = clamp(start.bounds.width - deltaX, bounds.minWidth, maxWidth);
      next.x = start.x + start.bounds.width - width;
      next.width = width;
    }
    if (corner.startsWith("north")) {
      const maxHeight = Math.min(bounds.maxHeight, Math.max(bounds.minHeight, start.y + start.bounds.height - WIDGET_EDGE_GAP));
      const height = clamp(start.bounds.height - deltaY, bounds.minHeight, maxHeight);
      next.y = start.y + start.bounds.height - height;
      next.height = height;
    }
    setGeometry(next);
  }

  function endInteraction(inputId: string) {
    const start = active.current;
    if (!start || start.inputId !== inputId) return;
    active.current = null;
    setInteracting(false);
  }

  function onPointerMove(event: ReactPointerEvent<HTMLElement>) {
    const inputId = `pointer:${event.pointerId}`;
    if (active.current?.inputId !== inputId) return;
    event.preventDefault();
    moveInteractionAt(event.clientX, event.clientY, inputId);
  }

  function onPointerEnd(event: ReactPointerEvent<HTMLElement>) {
    const inputId = `pointer:${event.pointerId}`;
    if (active.current?.inputId !== inputId) return;
    endInteraction(inputId);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  spatialHandler.current = (interaction) => {
    const inputId = "airhand";
    if (interaction.phase === "begin") {
      if (!interaction.kind) return;
      beginInteractionAt(interaction.kind, interaction.corner, interaction.clientX, interaction.clientY, inputId);
      return;
    }
    if (interaction.phase === "move") {
      moveInteractionAt(interaction.clientX, interaction.clientY, inputId, interaction.scale);
      return;
    }
    endInteraction(inputId);
  };

  useEffect(() => subscribeVoiceSpatialInteraction(documentId, (interaction) => {
    spatialHandler.current(interaction);
  }), [documentId]);

  useEffect(() => {
    if (!placement) return;
    const reset = (event: Event) => {
      if (!(event instanceof CustomEvent) || event.detail?.accessProfile !== accessProfile) return;
      active.current = null;
      setInteracting(false);
      const element = ref.current;
      const stage = element?.closest<HTMLElement>(".voice-widget-canvas");
      const stored = readWidgetGeometry(documentId, accessProfile);
      const next = element && stage ? initializeWidgetGeometry(element, stage, stored, placement, sizePolicy) : stored;
      if (element && stage) widgetStageGeometries.get(stage)?.set(element, next);
      setGeometry(next);
    };
    window.addEventListener(VOICE_WIDGET_LAYOUT_RESET_EVENT, reset);
    return () => window.removeEventListener(VOICE_WIDGET_LAYOUT_RESET_EVENT, reset);
  }, [accessProfile, documentId, placement?.count, placement?.index, placement?.slot, sizePolicy]);

  const style = useMemo<CSSProperties | undefined>(() => {
    if (!placement) return undefined;
    const width = geometry.width ?? sizePolicy.preferredWidth ?? undefined;
    const height = geometry.height ?? sizePolicy.preferredHeight ?? undefined;
    return {
      height: height == null ? undefined : `${height}px`,
      left: `${geometry.x ?? 0}px`,
      top: `${geometry.y ?? 0}px`,
      visibility: positioned ? "visible" : "hidden",
      width: width == null ? undefined : `${width}px`,
      zIndex: geometry.z,
    };
  }, [geometry, placement?.slot, positioned, sizePolicy]);

  return {
    corners: VOICE_WIDGET_RESIZE_CORNERS,
    interacting,
    layoutReady,
    onDragPointerDown,
    onPointerEnd,
    onPointerMove,
    onResizePointerDown,
    ref,
    style,
  };
}

function constrainWidgetGeometry(
  element: HTMLElement,
  stage: HTMLElement,
  geometry: WidgetGeometry,
  policy: WidgetSizePolicy,
): WidgetGeometry {
  const stageBounds = stage.getBoundingClientRect();
  if (!validWidgetStage(stageBounds)) return geometry;
  const bounds = widgetSizeBounds(policy, stageBounds.width, stageBounds.height, element);
  const renderedWidth = clamp(geometry.width ?? element.offsetWidth, bounds.minWidth, bounds.maxWidth);
  const renderedHeight = clamp(geometry.height ?? element.offsetHeight, bounds.minHeight, bounds.maxHeight);
  const width = geometry.width == null && renderedWidth === element.offsetWidth ? null : renderedWidth;
  const height = geometry.height == null && renderedHeight === element.offsetHeight ? null : renderedHeight;
  const x = responsiveWidgetCoordinate(
    geometry.x,
    geometry.stageWidth,
    geometry.width ?? renderedWidth,
    stageBounds.width,
    renderedWidth,
  );
  const y = responsiveWidgetCoordinate(
    geometry.y,
    geometry.stageHeight,
    geometry.height ?? renderedHeight,
    stageBounds.height,
    renderedHeight,
  );
  if (
    width === geometry.width
    && height === geometry.height
    && x === geometry.x
    && y === geometry.y
    && stageBounds.width === geometry.stageWidth
    && stageBounds.height === geometry.stageHeight
  ) return geometry;
  return {
    ...geometry,
    height,
    stageHeight: stageBounds.height,
    stageWidth: stageBounds.width,
    width,
    x,
    y,
  };
}

function widgetSizePolicy(layout?: VoiceWidgetLayout): WidgetSizePolicy {
  if (!layout) {
    return {
      minHeight: WIDGET_MIN_HEIGHT,
      minWidth: WIDGET_MIN_WIDTH,
      preferredHeight: null,
      preferredWidth: null,
      maxHeight: null,
      maxWidth: null,
    };
  }
  return {
    minHeight: layout.min_height,
    minWidth: layout.min_width,
    preferredHeight: layout.preferred_height,
    preferredWidth: layout.preferred_width,
    maxHeight: layout.max_height,
    maxWidth: layout.max_width,
  };
}

function widgetSizeBounds(
  policy: WidgetSizePolicy,
  stageWidth: number,
  stageHeight: number,
  element: HTMLElement,
): WidgetSizeBounds {
  const stageMaxWidth = Math.max(1, stageWidth - WIDGET_EDGE_GAP * 2);
  const stageMaxHeight = Math.max(1, stageHeight - WIDGET_EDGE_GAP * 2);
  const minWidth = Math.min(stageMaxWidth, policy.minWidth);
  const minHeight = Math.min(stageMaxHeight, policy.minHeight);
  const maxWidth = Math.max(minWidth, Math.min(stageMaxWidth, policy.maxWidth ?? stageMaxWidth));
  const maxHeight = Math.max(minHeight, Math.min(stageMaxHeight, policy.maxHeight ?? stageMaxHeight));
  return {
    minHeight,
    minWidth,
    preferredHeight: clamp(policy.preferredHeight ?? element.offsetHeight, minHeight, maxHeight),
    preferredWidth: clamp(policy.preferredWidth ?? element.offsetWidth, minWidth, maxWidth),
    maxHeight,
    maxWidth,
  };
}

function initializeWidgetGeometry(
  element: HTMLElement,
  stage: HTMLElement,
  geometry: WidgetGeometry,
  placement: VoiceSurfacePlacement,
  policy: WidgetSizePolicy,
): WidgetGeometry {
  const stageBounds = stage.getBoundingClientRect();
  if (!validWidgetStage(stageBounds)) return geometry;
  const current = geometry.slot === placement.slot ? geometry : { ...geometry, slot: placement.slot };
  if (current.x != null && current.y != null) {
    return constrainWidgetGeometry(element, stage, current, policy);
  }
  const bounds = widgetSizeBounds(policy, stageBounds.width, stageBounds.height, element);
  const width = clamp(current.width ?? bounds.preferredWidth, bounds.minWidth, bounds.maxWidth);
  const height = clamp(current.height ?? bounds.preferredHeight, bounds.minHeight, bounds.maxHeight);
  const localRect = (node: HTMLElement): WidgetRect => {
    const rect = node.getBoundingClientRect();
    return { x: rect.left - stageBounds.left, y: rect.top - stageBounds.top, width: rect.width, height: rect.height };
  };
  const occupied = [...stage.querySelectorAll<HTMLElement>(":scope > [data-voice-slot]")]
    .filter((peer) => peer !== element && !peer.hasAttribute("data-voice-leaving"))
    .flatMap((peer): WidgetRect[] => {
      const saved = widgetStageGeometries.get(stage)?.get(peer);
      if (saved?.x != null && saved.y != null) return [{ x: saved.x, y: saved.y, width: saved.width ?? peer.offsetWidth, height: saved.height ?? peer.offsetHeight }];
      return peer.style.visibility === "visible" ? [localRect(peer)] : [];
    });
  const protectedAreas = [...(stage.closest(".home-view-transition") ?? stage.closest(".voice-view"))?.querySelectorAll<HTMLElement>("[data-voice-placement-obstacle]") ?? []]
    .map(localRect).filter((rect) => rect.width > 0 && rect.height > 0);
  const { x, y } = placeVoiceWidget(stageBounds, { width, height }, occupied, protectedAreas);
  return constrainWidgetGeometry(element, stage, {
    ...current,
    height,
    stageHeight: stageBounds.height,
    stageWidth: stageBounds.width,
    width,
    x,
    y,
    z: ++voiceWidgetZ,
  }, policy);
}

function responsiveWidgetCoordinate(
  coordinate: number | null,
  previousStageSize: number | null,
  previousWidgetSize: number,
  stageSize: number,
  widgetSize: number,
): number {
  const nextTravel = Math.max(0, stageSize - widgetSize - WIDGET_EDGE_GAP * 2);
  if (coordinate == null) return WIDGET_EDGE_GAP;
  if (previousStageSize == null || Math.abs(previousStageSize - stageSize) < 0.5) {
    return clamp(coordinate, WIDGET_EDGE_GAP, WIDGET_EDGE_GAP + nextTravel);
  }
  const previousTravel = Math.max(0, previousStageSize - previousWidgetSize - WIDGET_EDGE_GAP * 2);
  const ratio = previousTravel > 0
    ? clamp((coordinate - WIDGET_EDGE_GAP) / previousTravel, 0, 1)
    : 0;
  return WIDGET_EDGE_GAP + ratio * nextTravel;
}

function validWidgetStage(bounds: DOMRect): boolean {
  return Number.isFinite(bounds.width)
    && Number.isFinite(bounds.height)
    && bounds.width > WIDGET_EDGE_GAP * 2
    && bounds.height > WIDGET_EDGE_GAP * 2;
}

function voiceWidgetLayoutStorageKey(accessProfile: OctopusClientAccessProfile): string {
  return accessProfile === "live" ? VOICE_WIDGET_LAYOUT_KEY : LAB_VOICE_WIDGET_LAYOUT_KEY;
}

function readWidgetGeometry(documentId: string, accessProfile: OctopusClientAccessProfile): WidgetGeometry {
  try {
    const stored = JSON.parse(localStorage.getItem(voiceWidgetLayoutStorageKey(accessProfile)) || "{}");
    const value = stored && typeof stored === "object" ? stored[documentId] : undefined;
    if (!value || typeof value !== "object") return { ...DEFAULT_WIDGET_GEOMETRY };
    const geometry = {
      height: finiteDimension(value.height),
      stageHeight: finiteDimension(value.stageHeight),
      stageWidth: finiteDimension(value.stageWidth),
      width: finiteDimension(value.width),
      x: finiteCoordinate(value.x),
      y: finiteCoordinate(value.y),
      z: finiteZ(value.z),
      slot: value.slot === "left" || value.slot === "right" ? value.slot : null,
    };
    voiceWidgetZ = Math.max(voiceWidgetZ, geometry.z);
    return geometry;
  } catch {
    return { ...DEFAULT_WIDGET_GEOMETRY };
  }
}

function writeWidgetGeometry(
  documentId: string,
  accessProfile: OctopusClientAccessProfile,
  geometry: WidgetGeometry,
) {
  try {
    const storageKey = voiceWidgetLayoutStorageKey(accessProfile);
    const stored = JSON.parse(localStorage.getItem(storageKey) || "{}");
    const layouts = stored && typeof stored === "object" && !Array.isArray(stored) ? stored : {};
    layouts[documentId] = geometry;
    localStorage.setItem(storageKey, JSON.stringify(layouts));
  } catch {}
}

function finiteDimension(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function finiteCoordinate(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? clamp(value, 0, 16384) : null;
}

function finiteZ(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? clamp(Math.floor(value), 1, 100000) : 1;
}

function useSurfaceData(admitted: AdmittedClientSurfaceDocument): DataState {
  const projection = useProjectionSurfaceData(admitted.document.sources ?? EMPTY_PROJECTION_SOURCES);
  const host = useHostSurfaceData(admitted.hostSources ?? EMPTY_HOST_SOURCES);
  return {
    loading: projection.loading || host.loading,
    sources: { ...projection.sources, ...host.sources },
    error: [projection.error, host.error].filter(Boolean).join(" · "),
  };
}

function useProjectionSurfaceData(sources: NonNullable<ClientSurfaceDocument["sources"]>): DataState {
  const generation = useProjectionGeneration();
  const hasSources = sources.length > 0;
  const [state, setState] = useState<DataState>({
    loading: hasSources,
    sources: {},
    error: "",
  });

  useEffect(() => {
    if (!sources.length) {
      setState({ loading: false, sources: {}, error: "" });
      return;
    }
    if (!hasProjectionBridge()) {
      setState({ loading: false, sources: {}, error: "Projection reader is unavailable" });
      return;
    }
    let cancelled = false;
    setState((current) => refreshState(current));
    void Promise.all(sources.map(async (source) => {
      const payload = await readProjection(source.method, source.params);
      return [source.id, source.path ? readPath(payload, source.path) : payload] as const;
    }))
      .then((entries) => {
        if (!cancelled) setState({ loading: false, sources: Object.fromEntries(entries), error: "" });
      })
      .catch((error) => {
        if (!cancelled) setState({ loading: false, sources: {}, error: formatProjectionError(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [generation, sources]);

  return state;
}

function useHostSurfaceData(sources: ClientSurfaceHostSource[]): DataState {
  const [state, setState] = useState<DataState>({
    loading: sources.length > 0,
    sources: {},
    error: "",
  });

  useEffect(() => {
    if (!sources.length) {
      setState({ loading: false, sources: {}, error: "" });
      return;
    }
    let cancelled = false;
    const refresh = () => {
      setState((current) => refreshState(current));
      void Promise.all(sources.map(async (source) => {
        if (source.kind === "filesystem") {
          if (!source.path) throw new Error(`Source "${source.id}" is not configured`);
          return [source.id, await readClientExtensionFilesystemSource({ path: source.path, read: source.read })] as const;
        }
        return [source.id, await readDeviceSource(source.device, source.deviceId)] as const;
      }))
        .then((entries) => {
          if (!cancelled) setState({ loading: false, sources: Object.fromEntries(entries), error: "" });
        })
        .catch((error) => {
          if (!cancelled) setState({ loading: false, sources: {}, error: formatProjectionError(error) });
        });
    };
    refresh();
    const refreshInterval = sources.reduce((minimum, source) => (
      source.kind === "filesystem" ? Math.min(minimum, source.refreshIntervalMs) : minimum
    ), Number.POSITIVE_INFINITY);
    const interval = Number.isFinite(refreshInterval) ? window.setInterval(refresh, refreshInterval) : 0;
    const refreshDevices = sources.some((source) => source.kind === "device") ? refresh : null;
    if (refreshDevices) navigator.mediaDevices?.addEventListener?.("devicechange", refreshDevices);
    return () => {
      cancelled = true;
      if (interval) window.clearInterval(interval);
      if (refreshDevices) navigator.mediaDevices?.removeEventListener?.("devicechange", refreshDevices);
    };
  }, [sources]);

  return state;
}

function refreshState(current: DataState): DataState {
  return {
    ...current,
    loading: Object.keys(current.sources).length === 0,
    error: "",
  };
}

async function readDeviceSource(device: "camera" | "microphone", selectedId: string): Promise<Record<string, unknown>> {
  if (!navigator.mediaDevices?.enumerateDevices) throw new Error(`${device} devices are unavailable in this client`);
  const kind: MediaDeviceKind = device === "camera" ? "videoinput" : "audioinput";
  const devices = (await navigator.mediaDevices.enumerateDevices())
    .filter((candidate) => candidate.kind === kind)
    .map((candidate) => ({
      device_id: candidate.deviceId,
      group_id: candidate.groupId,
      label: candidate.label,
    }));
  const selected = devices.find((candidate) => candidate.device_id === selectedId) ?? devices[0] ?? null;
  return {
    kind: device,
    available: devices.length > 0,
    selected_device_id: selected?.device_id ?? "",
    selected_label: selected?.label ?? "",
    devices,
  };
}

function NativeNode({ context, documentId, node, onAction, path }: {
  context: Record<string, unknown>;
  documentId: string;
  node: ClientSurfaceNode;
  onAction?: Props["onAction"];
  path: string;
}) {
  if (node.type === "stack" || node.type === "row") {
    const classes = [
      "client-surface-layout",
      `is-${node.type}`,
      `gap-${node.gap ?? "sm"}`,
      `align-${node.align ?? "stretch"}`,
    ];
    if (node.type === "row") {
      classes.push(`justify-${node.justify ?? "start"}`);
      if (node.wrap) classes.push("is-wrapping");
    }
    return (
      <div className={classes.join(" ")}>
        {node.children.map((child, index) => (
          <NativeNode
            context={context}
            documentId={documentId}
            key={`${path}:${index}`}
            node={child}
            onAction={onAction}
            path={`${path}.${index}`}
          />
        ))}
      </div>
    );
  }
  if (node.type === "section") {
    return (
      <section className="client-surface-section">
        <header>
          {node.glyph ? <Glyph name={node.glyph} size={14} /> : null}
          <span>{resolveText(node.title, context)}</span>
        </header>
        <div>
          {node.children.map((child, index) => (
            <NativeNode
              context={context}
              documentId={documentId}
              key={`${path}:${index}`}
              node={child}
              onAction={onAction}
              path={`${path}.${index}`}
            />
          ))}
        </div>
      </section>
    );
  }
  if (node.type === "text") {
    return (
      <span className={`client-surface-text role-${node.role ?? "body"} tone-${node.tone ?? "default"}${node.truncate ? " is-truncated" : ""}`}>
        {resolveText(node.value, context)}
      </span>
    );
  }
  if (node.type === "key_value") {
    return (
      <div className={`client-surface-key-value voice-fact-row tone-${node.tone ?? "default"}`}>
        <span>{resolveText(node.label, context)}</span>
        <strong title={resolveText(node.value, context)}>{resolveText(node.value, context)}</strong>
      </div>
    );
  }
  if (node.type === "status") {
    return (
      <div className={`client-surface-status voice-list-row tone-${node.tone ?? "default"}`}>
        <span className="voice-list-icon">{node.glyph ? <Glyph name={node.glyph} size={14} /> : null}</span>
        <span>
          {resolveText(node.label, context)}
          {node.detail ? <em>{resolveText(node.detail, context)}</em> : null}
        </span>
        {node.state ? <small>{resolveText(node.state, context)}</small> : null}
      </div>
    );
  }
  if (node.type === "metric") {
    return (
      <div className={`client-surface-metric tone-${node.tone ?? "default"}`}>
        <span>{resolveText(node.label, context)}</span>
        <strong>{resolveText(node.value, context)}</strong>
        {node.detail ? <small>{resolveText(node.detail, context)}</small> : null}
      </div>
    );
  }
  if (node.type === "progress") {
    const value = clamp(resolveNumber(node.value, context), 0, 100);
    return (
      <div className={`client-surface-progress tone-${node.tone ?? "default"}`}>
        {(node.label || node.detail) ? (
          <header>
            <span>{node.label ? resolveText(node.label, context) : ""}</span>
            <small>{node.detail ? resolveText(node.detail, context) : `${Math.round(value)}%`}</small>
          </header>
        ) : null}
        <div><i style={{ width: `${value}%` }} /></div>
      </div>
    );
  }
  if (node.type === "donut") {
    return (
      <DonutChart
        label={node.label ? resolveText(node.label, context) : "Complete"}
        segments={node.segments.map((segment) => ({
          label: resolveText(segment.label, context),
          tone: segment.tone ?? "default",
          value: Math.max(0, resolveNumber(segment.value, context)),
        }))}
        value={clamp(resolveNumber(node.value, context), 0, 100)}
      />
    );
  }
  if (node.type === "sparkline") {
    return <Sparkline label={node.label ? resolveText(node.label, context) : ""} tone={node.tone} values={resolveNumberList(node.values, context)} />;
  }
  if (node.type === "list") {
    const items = resolveCollection(node.items.path, node.items.limit, context);
    if (!items.length) {
      return <div className="client-surface-collection-empty">{node.empty ? resolveText(node.empty, context) : "No items"}</div>;
    }
    return (
      <div className={`client-surface-list${node.divided ? " is-divided" : ""}`} role="list">
        {items.map((item, index) => {
          const itemContext = { ...context, item, index };
          return (
            <div className="client-surface-list__item" key={`${path}:item:${index}`} role="listitem">
              {node.children.map((child, childIndex) => (
                <NativeNode
                  context={itemContext}
                  documentId={documentId}
                  key={`${path}:item:${index}:${childIndex}`}
                  node={child}
                  onAction={onAction}
                  path={`${path}.item.${index}.${childIndex}`}
                />
              ))}
            </div>
          );
        })}
      </div>
    );
  }
  if (node.type === "table") {
    const rows = resolveCollection(node.rows.path, node.rows.limit, context);
    if (!rows.length) {
      return <div className="client-surface-collection-empty">{node.empty ? resolveText(node.empty, context) : "No rows"}</div>;
    }
    return (
      <div className="client-surface-table-wrap">
        <table className="client-surface-table">
          <thead>
            <tr>
              {node.columns.map((column) => (
                <th className={`align-${column.align ?? "start"}`} key={column.key}>{resolveText(column.label, context)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, index) => {
              const rowContext = { ...context, row, index };
              return (
                <tr key={`${path}:row:${index}`}>
                  {node.columns.map((column) => (
                    <td className={`align-${column.align ?? "start"} role-${column.role ?? "text"}`} key={column.key}>
                      {resolveText(column.value, rowContext)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    );
  }
  if (node.type === "video") return <ClientSurfaceVideo key={node.source} source={node.source} title={node.title} />;
  if (node.type === "image") {
    const source = resolveText(node.source, context);
    const alt = node.alt ? resolveText(node.alt, context) : "";
    if (!isSafeClientSurfaceImageSource(source)) {
      return <ClientSurfaceState title="Image unavailable" detail="The source is not an admitted embedded image" />;
    }
    return (
      <div className={`client-surface-image ratio-${node.ratio ?? "landscape"}`}>
        <img alt={alt} draggable={false} src={source} style={{ objectFit: node.fit ?? "contain" }} />
      </div>
    );
  }
  if (node.type === "input") {
    return <NativeInput context={context} documentId={documentId} node={node} onAction={onAction} path={path} />;
  }
  if (node.type === "button") {
    if (node.variant === "footer") {
      return (
        <button
          className="voice-card-footer"
          onClick={() => dispatchAction(documentId, node.action, onAction)}
          type="button"
        >
          <span>{resolveText(node.label, context)}</span>
          {node.glyph ? <Glyph className="-rotate-90" name={node.glyph} size={12} /> : null}
        </button>
      );
    }
    return (
      <button
        className={`client-surface-button is-${node.variant ?? "quiet"}`}
        onClick={() => dispatchAction(documentId, node.action, onAction)}
        type="button"
      >
        <span>{resolveText(node.label, context)}</span>
        {node.glyph ? <Glyph name={node.glyph} size={12} /> : null}
      </button>
    );
  }
  if (node.type === "divider") return <div className="client-surface-divider" />;
  return <div className={`client-surface-spacer size-${node.size ?? "sm"}`} />;
}

function NativeInput({ context, documentId, node, onAction, path }: {
  context: Record<string, unknown>;
  documentId: string;
  node: Extract<ClientSurfaceNode, { type: "input" }>;
  onAction?: Props["onAction"];
  path: string;
}) {
  const initialValue = node.value ? resolveText(node.value, context) : "";
  const [value, setValue] = useState(initialValue);

  useEffect(() => {
    setValue(initialValue);
  }, [initialValue]);

  const label = node.label ? resolveText(node.label, context) : "";
  const placeholder = node.placeholder ? resolveText(node.placeholder, context) : "";
  return (
    <label className="client-surface-input">
      {label ? <span>{label}</span> : null}
      <input
        aria-label={label || placeholder || "Extension input"}
        onChange={(event) => setValue(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" || !node.action) return;
          event.preventDefault();
          dispatchAction(documentId, node.action, onAction, { path, value });
        }}
        placeholder={placeholder}
        type={node.input_kind ?? "text"}
        value={value}
      />
    </label>
  );
}

function DonutChart({ label, segments, value }: {
  label: string;
  segments: Array<{ label: string; tone: ClientSurfaceTone; value: number }>;
  value: number;
}) {
  const progress = Math.min(100, Math.max(0, value));
  return (
    <div className="client-surface-donut">
      <div className="client-surface-donut__legend">
        {segments.map((segment, index) => (
          <span key={`${segment.label}:${index}`}>
            <i className={`tone-${segment.tone}`} />
            <em>{segment.label}</em>
            <strong>{segment.value}</strong>
          </span>
        ))}
      </div>
      <div className="client-surface-donut__chart">
        <svg aria-hidden="true" viewBox="0 0 68 68">
          <circle className="client-surface-donut__track" cx="34" cy="34" fill="none" r="28" />
          {progress > 0 ? <circle
            className="client-surface-donut__ring"
            cx="34"
            cy="34"
            fill="none"
            pathLength="100"
            r="28"
            strokeDasharray={progress < 100 ? `${progress} 100` : undefined}
          /> : null}
        </svg>
        <span><strong>{Math.round(progress)}%</strong><small>{label}</small></span>
      </div>
    </div>
  );
}

function Sparkline({ label, tone = "default", values }: { label: string; tone?: ClientSurfaceTone; values: number[] }) {
  const paths = useMemo(() => {
    if (values.length < 2) return null;
    const min = Math.min(...values);
    const max = Math.max(...values);
    const range = max - min || 1;
    const points = values.map((value, index) => ({
      x: (index / (values.length - 1)) * 100,
      y: 26 - ((value - min) / range) * 20,
    }));
    let line = `M ${points[0].x.toFixed(2)} ${points[0].y.toFixed(2)}`;
    for (let index = 1; index < points.length; index += 1) {
      const previous = points[index - 1];
      const current = points[index];
      const midpoint = ((previous.x + current.x) / 2).toFixed(2);
      line += ` C ${midpoint} ${previous.y.toFixed(2)}, ${midpoint} ${current.y.toFixed(2)}, ${current.x.toFixed(2)} ${current.y.toFixed(2)}`;
    }
    const area = `${line} L ${points[points.length - 1].x.toFixed(2)} 32 L ${points[0].x.toFixed(2)} 32 Z`;
    return { area, line };
  }, [values]);
  return (
    <div className={`client-surface-sparkline tone-${tone}`}>
      {label ? <span>{label}</span> : null}
      {paths ? (
        <svg aria-hidden="true" preserveAspectRatio="none" viewBox="0 0 100 32">
          <defs>
            <linearGradient id="client-surface-sparkline-area" x1="0" x2="0" y1="0" y2="1">
              <stop className="client-surface-sparkline__area-stop-a" offset="0" />
              <stop className="client-surface-sparkline__area-stop-b" offset="1" />
            </linearGradient>
          </defs>
          <path className="client-surface-sparkline__area" d={paths.area} />
          <path className="client-surface-sparkline__line" d={paths.line} />
        </svg>
      ) : <small>No series</small>}
    </div>
  );
}

function SandboxSurface({ admitted, appearance, context, onAction }: {
  admitted: AdmittedClientSurfaceDocument;
  appearance: AppearanceSettings;
  context: Record<string, unknown>;
  onAction?: Props["onAction"];
}) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const render = admitted.document.render;
  const [height, setHeight] = useState(render.kind === "sandbox" ? render.initial_height ?? 240 : 240);
  const source = useMemo(
    () => render.kind === "sandbox" ? sandboxDocument(render, context) : "",
    [appearance, context, render],
  );

  useEffect(() => {
    const listener = (event: MessageEvent) => {
      if (event.source !== iframeRef.current?.contentWindow) return;
      if (!event.data || typeof event.data !== "object") return;
      if (event.data.type === "octopus.surface.resize") {
        const next = Number(event.data.height);
        if (Number.isFinite(next)) setHeight(clamp(next, 80, 1600));
        return;
      }
      if (event.data.type === "octopus.surface.error") {
        reportClientSurfaceRuntimeIssue({
          extensionId: admitted.sourceExtensionId,
          documentId: admitted.document.id,
          sourceDigest: admitted.sourceDigest,
        }, "script", typeof event.data.message === "string" ? event.data.message : "Sandbox script failed");
        return;
      }
      if (event.data.type !== "octopus.surface.action" || render.kind !== "sandbox") return;
      const actionId = typeof event.data.actionId === "string" ? event.data.actionId : "";
      const action = render.actions?.[actionId];
      if (action) dispatchAction(admitted.document.id, action, onAction);
    };
    window.addEventListener("message", listener);
    return () => window.removeEventListener("message", listener);
  }, [admitted.document.id, admitted.sourceDigest, admitted.sourceExtensionId, onAction, render]);

  return (
    <iframe
      className="client-surface-sandbox"
      key={admitted.sourceDigest}
      ref={iframeRef}
      sandbox="allow-scripts"
      srcDoc={source}
      style={{ height }}
      title={admitted.document.title}
    />
  );
}

function sandboxDocument(render: Extract<ClientSurfaceDocument["render"], { kind: "sandbox" }>, context: Record<string, unknown>): string {
  const encodedContext = encodeScriptJson(context);
  const css = escapeClosingTag(`${sandboxThemeCss()}${render.css ?? ""}`, "style");
  const javascript = escapeClosingTag(render.javascript ?? "", "script");
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob:; media-src 'none'; font-src data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"><meta name="viewport" content="width=device-width,initial-scale=1"><style>${css}</style></head><body>${render.html}<script>(()=>{const fail=(reason)=>parent.postMessage({type:"octopus.surface.error",message:String(reason&&reason.message||reason||"Script error").slice(0,2000)},"*");addEventListener("error",(event)=>fail(event.error||event.message));addEventListener("unhandledrejection",(event)=>fail(event.reason))})();window.octopusSurface=${encodedContext};Object.defineProperty(window.octopusSurface,"invoke",{enumerable:false,value:(actionId)=>{if(typeof actionId==="string"&&/^[a-z0-9][a-z0-9._-]{1,63}$/.test(actionId))parent.postMessage({type:"octopus.surface.action",actionId},"*")}});(()=>{const send=()=>parent.postMessage({type:"octopus.surface.resize",height:Math.ceil(document.documentElement.getBoundingClientRect().height)},"*");new ResizeObserver(send).observe(document.documentElement);addEventListener("load",send);send()})();</script>${javascript ? `<script>${javascript}</script>` : ""}</body></html>`;
}

function sandboxThemeCss(): string {
  const root = globalThis.document.documentElement;
  const style = getComputedStyle(root);
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  const colorScheme = root.dataset.theme === "light" ? "light" : "dark";
  const backgroundPrimary = token("--color-token-main-surface-primary", colorScheme === "light" ? "#ffffff" : "#181818");
  const backgroundSecondary = token("--color-token-bg-secondary", colorScheme === "light" ? "#f3f3f3" : "#212121");
  const textPrimary = token("--color-token-foreground", colorScheme === "light" ? "#171717" : "#ffffff");
  const textSecondary = token("--color-token-text-secondary", colorScheme === "light" ? "rgb(0 0 0 / 62%)" : "rgb(255 255 255 / 62%)");
  const textTertiary = token("--color-token-text-tertiary", colorScheme === "light" ? "rgb(0 0 0 / 44%)" : "rgb(255 255 255 / 44%)");
  const border = token("--color-token-border", colorScheme === "light" ? "rgb(0 0 0 / 10%)" : "rgb(255 255 255 / 9%)");
  const accent = token("--color-token-text-link-foreground", "#339cff");
  const danger = token("--color-token-error-foreground", "#fa423e");
  const fontSans = token("--font-ui", '-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif');
  const fontMono = token("--font-mono", 'ui-monospace, "SFMono-Regular", Menlo, monospace');
  const textBase = token("--text-base", "14px");
  const textSmall = token("--text-sm", "12px");
  const textExtraSmall = token("--text-xs", "11px");
  const radius = token("--radius-lg", "10px");
  const scrollbarTokens = ["background", "hover-background", "active-background"]
    .map((state) => {
      const name = `--color-token-scrollbar-slider-${state}`;
      return `${name}:${token(name, border)}`;
    }).join(";");
  return `:root{color-scheme:${colorScheme};--color-background-primary:${backgroundPrimary};--color-background-secondary:${backgroundSecondary};--color-text-primary:${textPrimary};--color-text-secondary:${textSecondary};--color-text-tertiary:${textTertiary};--color-border-primary:${border};--color-accent:${accent};--color-danger:${danger};--font-sans:${fontSans};--font-mono:${fontMono};--text-base:${textBase};--text-sm:${textSmall};--text-xs:${textExtraSmall};--border-radius-md:${radius};${scrollbarTokens}}*{box-sizing:border-box}html,body{min-height:100%;margin:0;background:transparent;color:var(--color-text-primary);font-family:var(--font-sans);font-size:var(--text-base);-webkit-font-smoothing:antialiased}${scrollbarCss}`;
}

function dispatchAction(
  documentId: string,
  action: ClientSurfaceAction,
  onAction?: Props["onAction"],
  input?: ClientSurfaceActionEvent["input"],
) {
  const detail = { documentId, action, ...(input ? { input } : {}) } satisfies ClientSurfaceActionEvent;
  const handled = onAction?.(detail) === true;
  if (!handled) window.dispatchEvent(new CustomEvent(CLIENT_SURFACE_ACTION_EVENT, { detail }));
}

function resolveText(value: ClientSurfaceValue, context: Record<string, unknown>): string {
  const resolved = resolveValue(value, context);
  if (resolved == null) return "";
  if (typeof resolved === "string") return resolved;
  if (typeof resolved === "number" || typeof resolved === "boolean") return String(resolved);
  try { return JSON.stringify(resolved); } catch { return ""; }
}

function resolveValue(value: ClientSurfaceValue, context: Record<string, unknown>): unknown {
  if (!isBinding(value)) return value;
  const resolved = readPath(context, value.path);
  const actual = resolved == null ? value.fallback : resolved;
  if (actual == null) return "";
  if (value.format === "number" && typeof actual === "number") return new Intl.NumberFormat().format(actual);
  if (value.format === "percent" && typeof actual === "number") return new Intl.NumberFormat(undefined, { style: "percent", maximumFractionDigits: 1 }).format(actual);
  if ((value.format === "date" || value.format === "time") && (typeof actual === "string" || typeof actual === "number")) {
    const date = new Date(actual);
    if (!Number.isNaN(date.valueOf())) {
      return value.format === "date" ? date.toLocaleDateString() : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    }
  }
  if (value.format === "json") {
    try { return JSON.stringify(actual); } catch { return ""; }
  }
  return actual;
}

function resolveNumber(value: ClientSurfaceNumberValue, context: Record<string, unknown>): number {
  if (typeof value === "number") return value;
  const resolved = resolveValue(value, context);
  return typeof resolved === "number" && Number.isFinite(resolved) ? resolved : Number(value.fallback ?? 0);
}

function resolveNumberList(value: ClientSurfaceNumberListValue, context: Record<string, unknown>): number[] {
  const candidate = Array.isArray(value) ? value : readPath(context, value.path);
  if (!Array.isArray(candidate)) return [];
  return candidate.filter((item): item is number => typeof item === "number" && Number.isFinite(item)).slice(0, 120);
}

function resolveCollection(path: string, limit: number | undefined, context: Record<string, unknown>): unknown[] {
  const candidate = readPath(context, path);
  if (!Array.isArray(candidate)) return [];
  return candidate.slice(0, clamp(
    Math.floor(limit ?? DEFAULT_CLIENT_SURFACE_COLLECTION_LIMIT),
    1,
    MAX_CLIENT_SURFACE_COLLECTION_LIMIT,
  ));
}

function isBinding(value: ClientSurfaceValue): value is ClientSurfaceBinding {
  return Boolean(value && typeof value === "object" && !Array.isArray(value) && typeof value.path === "string");
}

function ClientSurfaceState({ detail, title }: { detail: string; title: string }) {
  return <div className="client-surface-state"><strong>{title}</strong><span>{detail}</span></div>;
}

function encodeScriptJson(value: unknown): string {
  return JSON.stringify(value).replace(/[<>&]/g, (character) => (
    character === "<" ? "\\u003c" : character === ">" ? "\\u003e" : "\\u0026"
  ));
}

function escapeClosingTag(value: string, tag: "script" | "style"): string {
  return value.replace(new RegExp(`</${tag}`, "gi"), `<\\/${tag}`);
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}
