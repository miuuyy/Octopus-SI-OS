import { OcSkeleton } from "../../design-system/LoadingSkeleton";
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { OcExpandableList, OcIconButton, cx } from "../../design-system/components";
import "./catalog.css";

export type CatalogInstalledRailProps<T> = {
  getItemKey: (item: T) => string;
  getItemLabel: (item: T) => string;
  items: readonly T[];
  manageLabel: string;
  onManage: () => void;
  onOpen: (item: T) => void;
  renderIcon: (item: T, size: "tile" | "mini") => ReactNode;
  title?: ReactNode;
};

export function CatalogInstalledRail<T>({
  getItemKey,
  getItemLabel,
  items,
  manageLabel,
  onManage,
  onOpen,
  renderIcon,
  title = "Installed",
}: CatalogInstalledRailProps<T>) {
  const containerRef = useRef<HTMLDivElement>(null);
  const reducedMotion = useReducedMotion();
  const [capacity, setCapacity] = useState(items.length);
  const shown = items.length > 0;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(([entry]) => {
      setCapacity(Math.max(1, Math.floor(entry.contentRect.width / 44)));
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, [shown]);

  const visibleCount = items.length > capacity ? Math.max(0, capacity - 1) : capacity;
  const visibleItems = items.slice(0, visibleCount);
  const hiddenItems = items.slice(visibleCount);
  return (
    <AnimatePresence initial={false}>
      {items.length ? (
        <motion.section
          animate={{ height: "auto", marginBottom: 0, opacity: 1, transitionEnd: { overflow: "visible" } }}
          className="flex flex-col gap-3"
          exit={{ height: 0, marginBottom: "-2rem", opacity: 0, overflow: "hidden" }}
          initial={{ height: 0, marginBottom: "-2rem", opacity: 0, overflow: "hidden" }}
          key="installed"
          transition={reducedMotion ? { duration: 0 } : { duration: 0.24, ease: [0.2, 0, 0, 1] }}
        >
          <div className="flex items-center justify-between gap-3 border-b border-border/40 px-2 pb-2">
            <h2 className="text-lg leading-6 font-medium text-default">{title}</h2>
            <OcIconButton glyph="settings" label={manageLabel} onClick={onManage} size="xs" variant="ghost" />
          </div>
          <div className="px-2">
            <div className="group/catalog-installed-row flex h-11 w-full justify-between overflow-visible" ref={containerRef}>
              {visibleItems.map((item) => {
                const itemLabel = getItemLabel(item);
                return (
                  <button
                    aria-label={itemLabel}
                    className="group relative flex size-11 shrink-0 cursor-interaction items-center justify-center rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0"
                    key={getItemKey(item)}
                    onClick={() => onOpen(item)}
                    type="button"
                  >
                    <span aria-hidden="true" className="flex size-9 shrink-0 items-center justify-center overflow-hidden rounded-lg bg-surface shadow-lg ring-1 ring-border ring-inset transition-transform duration-basic ease-out group-hover:-translate-y-1 group-focus-visible:translate-y-0 motion-reduce:transition-none">
                      {renderIcon(item, "tile")}
                    </span>
                    <span aria-hidden="true" className="pointer-events-none absolute start-1/2 top-full z-10 -translate-x-1/2 text-xs leading-4 whitespace-nowrap text-secondary opacity-0 transition-opacity duration-basic ease-out group-hover:opacity-100 group-focus-visible:opacity-100 group-has-[:focus-visible]/catalog-installed-row:translate-y-1 group-has-[:focus-visible]/catalog-installed-row:duration-0 motion-reduce:transition-none">
                      {itemLabel}
                    </span>
                  </button>
                );
              })}
              {hiddenItems.length ? (
                <button
                  aria-label={`${hiddenItems.length} more. ${manageLabel}`}
                  className="group relative flex size-11 shrink-0 cursor-interaction items-center justify-center rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0"
                  onClick={onManage}
                  type="button"
                >
                  <span aria-hidden="true" className="flex size-9 shrink-0 items-center justify-center overflow-hidden rounded-lg bg-surface shadow-lg ring-1 ring-border ring-inset transition-transform duration-basic ease-out group-hover:-translate-y-1 group-focus-visible:translate-y-0 motion-reduce:transition-none">
                    <span className="grid grid-cols-2 gap-0.5">
                      {hiddenItems.slice(0, 4).map((item) => (
                        <span className="flex size-[13px] items-center justify-center overflow-hidden rounded-[4px] bg-surface ring-1 ring-border ring-inset" key={getItemKey(item)}>
                          {renderIcon(item, "mini")}
                        </span>
                      ))}
                    </span>
                  </span>
                  <span aria-hidden="true" className="pointer-events-none absolute start-1/2 top-full z-10 -translate-x-1/2 text-xs leading-4 whitespace-nowrap text-secondary opacity-0 transition-opacity duration-basic ease-out group-hover:opacity-100 group-focus-visible:opacity-100 group-has-[:focus-visible]/catalog-installed-row:translate-y-1 group-has-[:focus-visible]/catalog-installed-row:duration-0 motion-reduce:transition-none">
                    {hiddenItems.length} more
                  </span>
                </button>
              ) : null}
            </div>
          </div>
        </motion.section>
      ) : null}
    </AnimatePresence>
  );
}

export type CatalogSectionProps<T> = {
  batchSize?: number;
  description?: ReactNode;
  getItemKey: (item: T) => string;
  getItemLabel: (item: T) => string;
  items: readonly T[];
  renderCard: (item: T) => ReactNode;
  renderItemPreview?: (item: T) => ReactNode;
  resetKey: string | number;
  showCount?: boolean;
  title: ReactNode;
  visibleItemLimit?: number;
};

export function CatalogSection<T>({
  batchSize = 12,
  description,
  getItemKey,
  getItemLabel,
  items,
  renderCard,
  renderItemPreview,
  resetKey,
  showCount = false,
  title,
  visibleItemLimit = 6,
}: CatalogSectionProps<T>) {
  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--padding-row-x)]">
        <div className="min-w-0">
          <h2 className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">
            {title}
            {showCount ? <span className="text-tertiary">{items.length}</span> : null}
          </h2>
          {description ? <p className="text-sm leading-5 text-secondary">{description}</p> : null}
        </div>
      </div>
      <OcExpandableList
        batchSize={batchSize}
        getItemKey={getItemKey}
        getItemLabel={getItemLabel}
        items={items}
        renderItemPreview={renderItemPreview}
        resetKey={resetKey}
        visibleItemLimit={visibleItemLimit}
      >
        {(visibleItems) => (
          <div className="oc-catalog-grid-container">
            <div className="oc-catalog-grid grid gap-x-6 gap-y-2">
              {visibleItems.map((item) => <Fragment key={getItemKey(item)}>{renderCard(item)}</Fragment>)}
            </div>
          </div>
        )}
      </OcExpandableList>
    </section>
  );
}

export type CatalogRouteTransitionProps = {
  children: ReactNode;
  className?: string;
  direction?: "back" | "forward";
  viewKey: string;
};

export function CatalogLoadingState({ label }: { label: string }) {
  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--padding-row-x)]">
        <h2 className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">
          <OcSkeleton size="sm" width={80} />
        </h2>
      </div>
      <div role="status">
        <span className="sr-only">{label}</span>
        <div className="oc-catalog-grid-container">
          <div className="oc-catalog-grid grid gap-x-6 gap-y-2">
            {[
              { key: "first", titleWidth: "w-20", descriptionWidth: "w-3/5" },
              { key: "second", titleWidth: "w-16", descriptionWidth: "w-1/2" },
              { key: "third", titleWidth: "w-24", descriptionWidth: "w-2/3" },
              { key: "fourth", titleWidth: "w-16", descriptionWidth: "w-2/5" },
              { key: "fifth", titleWidth: "w-20", descriptionWidth: "w-3/5" },
              { key: "sixth", titleWidth: "w-16", descriptionWidth: "w-1/2" },
            ].map((row) => (
              <div aria-hidden="true" key={row.key}>
                <div className="pointer-events-none flex flex-col justify-center gap-2.5 rounded-2xl border-none p-2 text-base" tabIndex={-1}>
                  <div className="flex items-center gap-3">
                    <span className="size-9 shrink-0"><OcSkeleton size="fill" /></span>
                    <div className="flex min-w-0 flex-1 items-center gap-3">
                      <div className="flex min-w-0 flex-1 flex-col justify-center gap-0.5">
                        <div className="flex min-w-0 items-center gap-2">
                          <div className="flex min-w-0 items-center gap-2">
                            <div className="truncate font-medium text-default"><OcSkeleton className={row.titleWidth} size="sm" /></div>
                          </div>
                        </div>
                        <div className="flex flex-col gap-0.5 text-sm leading-relaxed text-text-description"><OcSkeleton className={row.descriptionWidth} size="sm" /></div>
                      </div>
                      <div className="flex shrink-0 items-center"><span className="h-token-button-composer w-14"><OcSkeleton size="fill" /></span></div>
                    </div>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

export function CatalogRouteTransition({
  children,
  className,
  direction = "forward",
  viewKey,
}: CatalogRouteTransitionProps) {
  const reducedMotion = useReducedMotion();
  const transition = reducedMotion ? { duration: 0 } : { duration: 0.2 };
  return (
    <AnimatePresence custom={direction} initial={false} mode="popLayout">
      <motion.div
        animate="visible"
        className={className}
        custom={direction}
        exit="exit"
        initial="hidden"
        key={viewKey}
        transition={transition}
        variants={{
          hidden: (routeDirection: "back" | "forward") => ({
            opacity: 0,
            transform: `translateY(${routeDirection === "forward" ? 4 : -4}px)`,
          }),
          visible: { opacity: 1, transform: "translateY(0)" },
          exit: (routeDirection: "back" | "forward") => ({
            opacity: 0,
            transform: `translateY(${routeDirection === "forward" ? -4 : 4}px)`,
          }),
        }}
      >
        {children}
      </motion.div>
    </AnimatePresence>
  );
}
