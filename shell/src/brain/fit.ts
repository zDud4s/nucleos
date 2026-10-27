import { zoomIdentity, type ZoomTransform } from "d3-zoom";

export interface Bounds {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

export interface Insets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/**
 * The transform that centres `bounds` in the free part of a `width` x `height` viewport (the
 * viewport minus `insets`, minus `pad` all round), scaled to fit but clamped to [minK, maxK] so a
 * tiny graph is not blown up and a huge one stays legible.
 */
export function fitTransform(
  bounds: Bounds,
  width: number,
  height: number,
  insets: Insets,
  pad: number,
  minK: number,
  maxK: number,
): ZoomTransform {
  const freeW = Math.max(width - insets.left - insets.right - 2 * pad, 1);
  const freeH = Math.max(height - insets.top - insets.bottom - 2 * pad, 1);
  const fitK = Math.min(freeW / Math.max(bounds.maxX - bounds.minX, 1), freeH / Math.max(bounds.maxY - bounds.minY, 1));
  const k = Math.max(minK, Math.min(maxK, fitK));
  const cx = (bounds.minX + bounds.maxX) / 2;
  const cy = (bounds.minY + bounds.maxY) / 2;
  const centreX = insets.left + pad + freeW / 2;
  const centreY = insets.top + pad + freeH / 2;
  return zoomIdentity.translate(centreX, centreY).scale(k).translate(-cx, -cy);
}
