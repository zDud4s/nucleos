import { useEffect, useState, type CSSProperties } from "react";

export interface ImageProps {
  /** The full-resolution source. */
  src: string;
  /** Accessible description. Empty string marks the image decorative. */
  alt: string;
  /**
   * A tiny (a few hundred bytes) data-URI or low-res URL shown blurred until `src` has loaded,
   * then cross-faded away. Without it, a neutral tinted box holds the space instead.
   */
  placeholder?: string;
  /** Intrinsic ratio, e.g. `"16 / 9"`, reserved so nothing jumps when the image lands. */
  aspectRatio?: string;
  /** Pixel width/height for fixed-size uses (avatars, thumbnails). */
  width?: number;
  height?: number;
  /** `"circle"` for avatars. Defaults to the shared `--radius-md`. */
  shape?: "rounded" | "circle";
  /** Defaults to `"lazy"`; pass `"eager"` for above-the-fold previews. */
  loading?: "lazy" | "eager";
  className?: string;
}

/**
 * An image with a blurred placeholder (#8): the box is sized before the bytes arrive, the
 * low-res stand-in shows blurred, and the real image fades in on load. A failed load leaves the
 * placeholder up (never a broken-image glyph). The fade is a CSS transition, so
 * `prefers-reduced-motion` collapses it to an instant swap.
 */
export function Image({
  src,
  alt,
  placeholder,
  aspectRatio,
  width,
  height,
  shape = "rounded",
  loading = "lazy",
  className,
}: ImageProps) {
  const [state, setState] = useState<"loading" | "loaded" | "failed">("loading");
  // A new src starts a new load.
  useEffect(() => setState("loading"), [src]);

  const style: CSSProperties = {};
  if (aspectRatio !== undefined) style.aspectRatio = aspectRatio;
  if (width !== undefined) style.width = `${width}px`;
  if (height !== undefined) style.height = `${height}px`;

  const cls = ["ui-image", shape === "circle" ? "ui-image-circle" : "", className ?? ""]
    .filter(Boolean)
    .join(" ");

  return (
    <span className={cls} style={style} data-state={state}>
      {placeholder !== undefined ? (
        <img className="ui-image-placeholder" src={placeholder} alt="" aria-hidden="true" />
      ) : null}
      {state === "failed" ? null : (
        <img
          className="ui-image-full"
          src={src}
          alt={alt}
          loading={loading}
          decoding="async"
          onLoad={() => setState("loaded")}
          onError={() => setState("failed")}
        />
      )}
    </span>
  );
}
