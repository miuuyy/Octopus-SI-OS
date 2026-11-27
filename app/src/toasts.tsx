import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type CSSProperties } from "react";
import { Glyph } from "./components/IconGlyphs";

export type AppToastLevel = "info" | "success" | "warning" | "danger";

export type AppToastAction = {
  label: string;
  onSelect: () => void;
};

export type AppToastOptions = {
  action?: AppToastAction;
  dedupeKey?: string;
};

type AppToast = {
  id: number;
  level: AppToastLevel;
  title: string;
  duration: number;
  isShown: boolean;
  action?: AppToastAction;
  count: number;
  dedupeKey: string;
  revision: number;
};

let nextToastId = 1;
let toasts: AppToast[] = [];
const listeners = new Set<() => void>();
const MAX_VISIBLE_TOASTS = 5;

function publish() {
  for (const listener of listeners) listener();
}

function showToast(level: AppToastLevel, title: string, duration = 5, options: AppToastOptions = {}) {
  const dedupeKey = options.dedupeKey?.trim() || `${level}:${title}`;
  const duplicate = toasts.find((toast) => toast.isShown && toast.dedupeKey === dedupeKey);
  if (duplicate) {
    const updated = {
      ...duplicate,
      action: options.action ?? duplicate.action,
      count: duplicate.count + 1,
      duration,
      revision: duplicate.revision + 1,
      title,
    };
    toasts = [updated, ...toasts.filter((toast) => toast.id !== duplicate.id)].slice(0, MAX_VISIBLE_TOASTS);
    publish();
    return { close: () => closeToast(updated.id) };
  }
  const id = nextToastId;
  nextToastId += 1;
  toasts = [{
    id,
    level,
    title,
    duration,
    isShown: true,
    action: options.action,
    count: 1,
    dedupeKey,
    revision: 0,
  }, ...toasts].slice(0, MAX_VISIBLE_TOASTS);
  publish();
  if ((level === "warning" || level === "danger") && typeof window !== "undefined") {
    void window.octopusTransport?.logClientEvent({
      level: level === "danger" ? "error" : "warning",
      event: "renderer.toast",
      message: title,
    }).catch(() => undefined);
  }
  return { close: () => closeToast(id) };
}

function closeToast(id: number) {
  if (!toasts.some((toast) => toast.id === id && toast.isShown)) return;
  toasts = toasts.map((toast) => toast.id === id ? { ...toast, isShown: false } : toast);
  publish();
}

function removeToast(id: number) {
  if (!toasts.some((toast) => toast.id === id)) return;
  toasts = toasts.filter((toast) => toast.id !== id);
  publish();
}

export const appToast = {
  info: (title: string, duration?: number, options?: AppToastOptions) => showToast("info", title, duration, options),
  success: (title: string, duration?: number, options?: AppToastOptions) => showToast("success", title, duration, options),
  warning: (title: string, duration?: number, options?: AppToastOptions) => showToast("warning", title, duration, options),
  danger: (title: string, duration?: number, options?: AppToastOptions) => showToast("danger", title, duration, options),
  closeAll: () => {
    if (!toasts.some((toast) => toast.isShown)) return;
    toasts = toasts.map((toast) => ({ ...toast, isShown: false }));
    publish();
  },
};

export function useAppErrorToast(error: string | null | undefined) {
  const previous = useRef("");
  useEffect(() => {
    if (!error) {
      previous.current = "";
      return;
    }
    if (previous.current === error) return;
    previous.current = error;
    appToast.danger(error);
  }, [error]);
}

export function AppToastViewport({ anchorToWorkspace = false }: { anchorToWorkspace?: boolean }) {
  const items = useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => toasts,
  );
  const [workspaceBounds, setWorkspaceBounds] = useState<{ left: number; width: number } | null>(null);

  useLayoutEffect(() => {
    if (!anchorToWorkspace) {
      setWorkspaceBounds(null);
      return;
    }
    const workspace = document.querySelector<HTMLElement>(".workspace-pane-primary");
    if (!workspace) return;
    const measure = () => {
      const rect = workspace.getBoundingClientRect();
      setWorkspaceBounds((current) => (
        current?.left === rect.left && current.width === rect.width
          ? current
          : { left: rect.left, width: rect.width }
      ));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(workspace);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [anchorToWorkspace]);

  const style = workspaceBounds ? {
    "--app-toast-anchor-left": `${workspaceBounds.left}px`,
    "--app-toast-anchor-width": `${workspaceBounds.width}px`,
  } as CSSProperties : undefined;

  return (
    <span className="app-toast-viewport" aria-live="polite" style={style}>
      {items.map((toast) => <AppToastItem key={toast.id} toast={toast} />)}
    </span>
  );
}

function AppToastItem({ toast }: { toast: AppToast }) {
  const [paused, setPaused] = useState(false);
  const remainingMs = useRef(toast.duration * 1_000);
  const timerStartedAt = useRef(0);

  useEffect(() => {
    remainingMs.current = toast.duration * 1_000;
  }, [toast.duration, toast.revision]);

  useEffect(() => {
    if (!toast.isShown || !toast.duration || paused) return;
    timerStartedAt.current = window.performance.now();
    const timer = window.setTimeout(() => closeToast(toast.id), remainingMs.current);
    return () => {
      window.clearTimeout(timer);
      remainingMs.current = Math.max(0, remainingMs.current - (window.performance.now() - timerStartedAt.current));
    };
  }, [paused, toast.duration, toast.id, toast.isShown, toast.revision]);

  useEffect(() => {
    if (toast.isShown) return;
    const timer = window.setTimeout(() => removeToast(toast.id), 250);
    return () => window.clearTimeout(timer);
  }, [toast.id, toast.isShown]);

  return (
    <div className="app-toast-root no-drag" data-state={toast.isShown ? "entered" : "exiting"}>
      <div
        className={`app-toast-alert is-${toast.level}`}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) setPaused(false);
        }}
        onFocus={() => setPaused(true)}
        onPointerEnter={() => setPaused(true)}
        onPointerLeave={() => setPaused(false)}
        role="alert"
      >
        {toast.level === "info" ? null : (
          <div className="app-toast-icon" aria-hidden="true">
            <ToastIcon level={toast.level} />
          </div>
        )}
        <div className="app-toast-content">
          <span>{toast.title}</span>
          {toast.count > 1 ? <span aria-label={`Repeated ${toast.count} times`} className="app-toast-count">{toast.count}</span> : null}
        </div>
        {toast.action ? (
          <button
            className="app-toast-action"
            onClick={() => {
              toast.action?.onSelect();
              closeToast(toast.id);
            }}
            type="button"
          >
            {toast.action.label}
          </button>
        ) : null}
        <button aria-label="Close" className="app-toast-close" onClick={() => closeToast(toast.id)} type="button">
          <Glyph name="close" size={16} />
        </button>
      </div>
    </div>
  );
}

function ToastIcon({ level }: { level: AppToastLevel }) {
  if (level === "danger") {
    return (
      <svg fill="none" height="16" viewBox="0 0 16 16" width="16">
        <path d="M8.08398 9.30859C8.49579 9.35064 8.81726 9.69822 8.81738 10.1211C8.81738 10.5724 8.45123 10.9383 8 10.9385C7.57705 10.9384 7.22945 10.617 7.1875 10.2051L7.18262 10.1211L7.1875 10.0381C7.22923 9.62595 7.57689 9.30383 8 9.30371L8.08398 9.30859Z" fill="currentColor" />
        <path d="M8 5.06152C8.28995 5.06152 8.52539 5.29696 8.52539 5.58691V7.75391C8.52522 8.04371 8.28984 8.27832 8 8.27832C7.71016 8.27832 7.47478 8.04371 7.47461 7.75391V5.58691C7.47461 5.29696 7.71005 5.06152 8 5.06152Z" fill="currentColor" />
        <path clipRule="evenodd" d="M8 1.47461C11.6037 1.47461 14.5254 4.39634 14.5254 8C14.5254 11.6037 11.6037 14.5254 8 14.5254C4.39634 14.5254 1.47461 11.6037 1.47461 8C1.47461 4.39634 4.39634 1.47461 8 1.47461ZM8 2.52539C4.97624 2.52539 2.52539 4.97624 2.52539 8C2.52539 11.0238 4.97624 13.4746 8 13.4746C11.0238 13.4746 13.4746 11.0238 13.4746 8C13.4746 4.97624 11.0238 2.52539 8 2.52539Z" fill="currentColor" fillRule="evenodd" />
      </svg>
    );
  }
  if (level === "success") {
    return (
      <svg fill="none" height="16" viewBox="0 0 16 16" width="16">
        <path d="M9.82617 5.80469C9.98754 5.56401 10.3139 5.49907 10.5547 5.66016C10.795 5.82162 10.8602 6.14803 10.6992 6.38867L7.98633 10.4346C7.70834 10.8488 7.11691 10.8958 6.77637 10.5312L5.35352 9.00684C5.15579 8.7949 5.16712 8.46247 5.37891 8.26465C5.59087 8.06696 5.92331 8.07818 6.12109 8.29004L7.30664 9.56152L9.82617 5.80469Z" fill="currentColor" />
        <path clipRule="evenodd" d="M8 1.47461C11.6037 1.47461 14.5254 4.39634 14.5254 8C14.5254 11.6037 11.6037 14.5254 8 14.5254C4.39634 14.5254 1.47461 11.6037 1.47461 8C1.47461 4.39634 4.39634 1.47461 8 1.47461ZM8 2.52539C4.97624 2.52539 2.52539 4.97624 2.52539 8C2.52539 11.0238 4.97624 13.4746 8 13.4746C11.0238 13.4746 13.4746 11.0238 13.4746 8C13.4746 4.97624 11.0238 2.52539 8 2.52539Z" fill="currentColor" fillRule="evenodd" />
      </svg>
    );
  }
  return (
    <svg fill="none" height="16" viewBox="0 0 16 16" width="16">
      <path d="M8 7.48633C8.28351 7.48633 8.51367 7.71649 8.51367 8V10.667C8.5135 10.9503 8.2834 11.1797 8 11.1797C7.7166 11.1797 7.4865 10.9503 7.48633 10.667V8C7.48633 7.71649 7.71649 7.48633 8 7.48633Z" fill="currentColor" />
      <path d="M8 4.90137C8.37814 4.9015 8.68544 5.20779 8.68555 5.58594C8.68528 5.96395 8.37804 6.27135 8 6.27148C7.62194 6.27138 7.31569 5.96396 7.31543 5.58594C7.31554 5.20778 7.62184 4.90147 8 4.90137Z" fill="currentColor" />
      <path clipRule="evenodd" d="M8 1.48633C11.5972 1.48633 14.5137 4.40279 14.5137 8C14.5137 11.5972 11.5972 14.5137 8 14.5137C4.40279 14.5137 1.48633 11.5972 1.48633 8C1.48633 4.40279 4.40279 1.48633 8 1.48633ZM8 2.51367C4.9698 2.51367 2.51367 4.9698 2.51367 8C2.51367 11.0302 4.9698 13.4863 8 13.4863C11.0302 13.4863 13.4863 11.0302 13.4863 8C13.4863 4.9698 11.0302 2.51367 8 2.51367Z" fill="currentColor" fillRule="evenodd" />
    </svg>
  );
}
