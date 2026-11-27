import { motion } from "motion/react";
import { useShouldReduceMotion } from "../../appearance";
import { MessageTooltip } from "./MessageTooltip";
import type { ContextUsage } from "./contextUsage";

export function ContextUsageIndicator({ usage }: { usage: ContextUsage | null }) {
  const reducedMotion = useShouldReduceMotion();
  if (!usage) return null;
  const clamped = !Number.isFinite(usage.percent) || usage.percent < 0 ? 0 : usage.percent > 100 ? 100 : usage.percent;
  const percent = Math.round(clamped);
  return (
    <MessageTooltip
      side="top"
      label={(
        <span className="flex w-38 flex-col gap-0.5 text-center">
          <span className="opacity-60">Context window:</span>
          <span className={percent >= 50 ? "opacity-60" : undefined}>
            {percent >= 50 ? `${percent}% full` : `${percent}% used (${100 - percent}% left)`}
          </span>
          <span>{Math.round(usage.usedTokens / 1000)}k / {Math.round(usage.contextWindow / 1000)}k tokens used</span>
        </span>
      )}
    >
      <span
        aria-label={`Context usage: ${percent}%`}
        className="inline-flex size-4 items-center justify-center text-token-description-foreground"
        role="img"
      >
        <svg aria-hidden="true" className="shrink-0" height="12" viewBox="0 0 12 12" width="12">
          <circle cx="6" cy="6" fill="none" opacity="0.16" r="5" stroke="currentColor" strokeWidth="2" />
          <motion.circle
            animate={{
              opacity: clamped === 0 ? 0 : 1,
              strokeDashoffset: 100 - clamped,
            }}
            cx="6"
            cy="6"
            fill="none"
            initial={false}
            pathLength={100}
            r="5"
            stroke="currentColor"
            strokeDasharray={100}
            strokeLinecap="round"
            strokeWidth="2"
            transform="rotate(-90 6 6)"
            transition={reducedMotion ? { duration: 0 } : { duration: 0.12, ease: "easeOut" }}
          />
        </svg>
      </span>
    </MessageTooltip>
  );
}
