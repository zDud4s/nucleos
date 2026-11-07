import qrcode from "qrcode-generator";

/**
 * A QR code for `value`, drawn as inline SVG paths: no image, no `dangerouslySetInnerHTML`, and
 * nothing the page's CSP has to allow. Dark modules are one path, so it stays small.
 */
export function QrCode({ value, label }: { value: string; label: string }) {
  const code = qrcode(0, "M");
  code.addData(value);
  code.make();
  const count = code.getModuleCount();
  const quiet = 2;
  let path = "";
  for (let row = 0; row < count; row += 1) {
    for (let col = 0; col < count; col += 1) {
      if (code.isDark(row, col)) path += `M${col + quiet} ${row + quiet}h1v1h-1z`;
    }
  }
  const size = count + quiet * 2;
  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${size} ${size}`}
      width={176}
      height={176}
      shapeRendering="crispEdges"
    >
      <rect width={size} height={size} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}
