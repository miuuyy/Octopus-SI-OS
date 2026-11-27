import { useShouldReduceMotion } from "../../appearance";
import { Glyph } from "../IconGlyphs";
import "./sidebarActivity.css";

export type SidebarTaskState = "running" | "failed" | "waiting" | "scheduled" | "unread";

const statusLabels: Record<SidebarTaskState, string> = {
  running: "Running",
  failed: "Failed",
  waiting: "Waiting",
  scheduled: "Scheduled",
  unread: "New",
};

const RUNNING_DOTS = Array.from({ length: 8 }, (_, index) => {
  const angle = (index / 8) * Math.PI * 2 - Math.PI / 2;
  return { cx: 7 + Math.cos(angle) * 4.6, cy: 7 + Math.sin(angle) * 4.6, opacity: 0.22 + (index / 7) * 0.78 };
});

export function SidebarTaskStatus({ state, label, text, detail }: {
  state: SidebarTaskState;
  label?: string;
  text?: string;
  detail?: string;
}) {
  const reduceMotion = useShouldReduceMotion();
  const accessibleLabel = label ?? statusLabels[state];
  return (
    <span
      aria-label={accessibleLabel}
      className="sidebar-task-status"
      data-reduce-motion={reduceMotion || undefined}
      data-state={state}
      role="img"
      title={detail ?? accessibleLabel}
    >
      {state === "running" ? (
        <svg aria-hidden="true" className="sidebar-task-status-orbit" viewBox="0 0 14 14">
          {RUNNING_DOTS.map((dot, index) => <circle cx={dot.cx} cy={dot.cy} fill="currentColor" key={index} opacity={dot.opacity} r="1.15" />)}
        </svg>
      ) : state === "failed" ? (
        <svg aria-hidden="true" className="sidebar-task-status-alert" fill="none" viewBox="0 0 16 16">
          <path d="M8.08398 9.30859C8.49579 9.35064 8.81726 9.69822 8.81738 10.1211C8.81738 10.5724 8.45123 10.9383 8 10.9385C7.57705 10.9384 7.22945 10.617 7.1875 10.2051L7.18262 10.1211L7.1875 10.0381C7.22923 9.62595 7.57689 9.30383 8 9.30371L8.08398 9.30859Z" fill="currentColor" />
          <path d="M8 5.06152C8.28995 5.06152 8.52539 5.29696 8.52539 5.58691V7.75391C8.52522 8.04371 8.28984 8.27832 8 8.27832C7.71016 8.27832 7.47478 8.04371 7.47461 7.75391V5.58691C7.47461 5.29696 7.71005 5.06152 8 5.06152Z" fill="currentColor" />
          <path clipRule="evenodd" d="M8 1.47461C11.6037 1.47461 14.5254 4.39634 14.5254 8C14.5254 11.6037 11.6037 14.5254 8 14.5254C4.39634 14.5254 1.47461 11.6037 1.47461 8C1.47461 4.39634 4.39634 1.47461 8 1.47461ZM8 2.52539C4.97624 2.52539 2.52539 4.97624 2.52539 8C2.52539 11.0238 4.97624 13.4746 8 13.4746C11.0238 13.4746 13.4746 11.0238 13.4746 8C13.4746 4.97624 11.0238 2.52539 8 2.52539Z" fill="currentColor" fillRule="evenodd" />
        </svg>
      ) : state === "scheduled" ? (
        <Glyph name="scheduled" size={12} />
      ) : (
        <span aria-hidden="true" className="sidebar-task-status-dot" />
      )}
      {text ? <span aria-hidden="true" className="sidebar-task-status-text">{text}</span> : null}
    </span>
  );
}
