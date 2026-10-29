import { cx } from "../../design-system/components";
import { Glyph, type GlyphName } from "../IconGlyphs";
import type { ApplicationRecord } from "./applicationModel";

export function ApplicationLogo({ application, className, logoSize }: { application: ApplicationRecord; className?: string; logoSize: number }) {
  const lightArtwork = application.logo_data_url?.trim();
  const darkArtwork = application.logo_dark_data_url?.trim();
  const artwork = lightArtwork || darkArtwork;
  const hasThemeArtwork = Boolean(lightArtwork && darkArtwork && lightArtwork !== darkArtwork);
  return (
    <span className={cx(
      "flex shrink-0 items-center justify-center text-secondary",
      className,
    )}>
      {artwork ? (
        <>
          <img alt="" className={cx("size-full rounded-2xs object-contain", hasThemeArtwork && "application-artwork-light")} draggable={false} src={artwork} />
          {hasThemeArtwork ? <img alt="" className="application-artwork-dark size-full rounded-2xs object-contain" draggable={false} src={darkArtwork} /> : null}
        </>
      ) : <Glyph name={applicationGlyph(application)} size={logoSize} />}
    </span>
  );
}

export function ApplicationMiniLogo({ application }: { application: ApplicationRecord }) {
  return <ApplicationLogo application={application} className="size-[13px] overflow-hidden rounded-[4px] bg-surface ring-1 ring-border ring-inset" logoSize={8} />;
}

export function OctopusConnectMark({ className }: { className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cx("block bg-white", className)}
      style={{
        WebkitMaskImage: "url(./octopus-logo.png)",
        WebkitMaskPosition: "center",
        WebkitMaskRepeat: "no-repeat",
        WebkitMaskSize: "contain",
        maskImage: "url(./octopus-logo.png)",
        maskPosition: "center",
        maskRepeat: "no-repeat",
        maskSize: "contain",
      }}
    />
  );
}

function applicationGlyph(application: ApplicationRecord): GlyphName {
  const known = new Set<GlyphName>(["plugins", "image", "voice", "file", "artifact", "interfaces", "binding", "chat", "send", "scheduled", "folder"]);
  const candidate = application.icon as GlyphName;
  if (known.has(candidate)) return candidate;
  if (application.icon === "message") return "chat";
  if (application.icon === "email") return "send";
  if (application.icon === "files") return "folder";
  if (application.icon === "calendar") return "scheduled";
  if (application.category === "plugin") return "plugins";
  if (application.category === "artifact") return "artifact";
  return "interfaces";
}
