import { useId, type ReactNode } from "react";
import { OcButton, OcDialog, OcIconButton, OcStatus } from "../../design-system/components";
import { ConnectLogoRow } from "./ApplicationLogo";

export type PackageInstallReview = {
  developer?: string;
  logo: ReactNode;
  name: string;
  note: string;
  packages: readonly { id: string; name: string; installed?: boolean }[];
};

export function PackageInstallReviewDialog({ onCancel, onConfirm, review }: {
  onCancel: () => void;
  onConfirm: () => void;
  review: PackageInstallReview | null;
}) {
  const titleId = useId();
  return (
    <OcDialog ariaLabelledBy={titleId} onClose={onCancel} open={Boolean(review)} size="setup">
      {review ? (
        <div className="relative flex min-h-0 flex-col gap-6 overflow-hidden px-6 pt-8 pb-6">
          <OcIconButton appearance="dialog-close" className="absolute top-4 right-4" glyph="close" label="Close" onClick={onCancel} />
          <div className="flex flex-col items-center gap-4">
            <ConnectLogoRow>{review.logo}</ConnectLogoRow>
            <div className="flex flex-col items-center gap-1 text-center">
              <h2 className="heading-dialog m-0 font-semibold text-default" id={titleId}>Install {review.name}</h2>
              {review.developer ? <div className="text-sm text-secondary">Developed by {review.developer}</div> : null}
            </div>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            <div className="rounded-2xl border border-default px-4 py-2">
              <div className="flex flex-col gap-2 py-3">
                <div className="text-sm font-medium text-default">Includes</div>
                {review.packages.map((entry) => (
                  <div className="flex min-h-6 items-center justify-between gap-4 text-sm text-secondary" key={entry.id}>
                    <span className="min-w-0 truncate">{entry.name}</span>
                    {entry.installed ? <OcStatus label="Installed" /> : null}
                  </div>
                ))}
                <div className="text-xs text-secondary">{review.note}</div>
              </div>
            </div>
          </div>
          <OcButton className="w-full justify-center rounded-full! px-5 py-2 text-base" onClick={onConfirm} size="md" variant="primary">Install {review.name}</OcButton>
        </div>
      ) : null}
    </OcDialog>
  );
}
