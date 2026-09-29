// Renders the MSIX logo images in `src-tauri/msix/Assets/` from the app icon,
// `src/mework-icon.svg`. The PNGs are committed; rerun this after the icon changes:
//
//   swift scripts/render-msix-assets.swift
//
// macOS only (AppKit draws the SVG). Every file carries a resource qualifier
// (`scale-*`, `targetsize-*`, `altform-*`), and `scripts/package-msix.mjs` indexes
// them into `resources.pri`, which is how Windows picks one for each display size.
// No unqualified copies: makepri takes a bare name for the scale-100 variant and
// refuses the pair as a conflict.
import AppKit

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let source = root.appendingPathComponent("src/mework-icon.svg")
let outDir = root.appendingPathComponent("src-tauri/msix/Assets")
guard let icon = NSImage(contentsOf: source) else { fatalError("cannot load \(source.path)") }

/// name, canvas width, canvas height, icon size — the icon is centred on a transparent canvas.
var images: [(String, Int, Int, Int)] = []
let scales = [(100, 1.0), (125, 1.25), (150, 1.5), (200, 2.0), (400, 4.0)]
for (scale, factor) in scales {
  let small = Int((44 * factor).rounded())
  images.append(("Square44x44Logo.scale-\(scale).png", small, small, small))
  // Start tiles keep a margin around the logo, as Windows' own tiles do.
  let tile = Int((150 * factor).rounded())
  images.append(("Square150x150Logo.scale-\(scale).png", tile, tile, tile * 2 / 3))
  let store = Int((50 * factor).rounded())
  images.append(("StoreLogo.scale-\(scale).png", store, store, store))
}
// Taskbar, Start list and file-type sizes; the icon has its own plate, so the
// unplated variants (no accent-coloured square behind it) are the same picture.
for size in [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256] {
  for suffix in ["", "_altform-unplated", "_altform-lightunplated"] {
    images.append(("Square44x44Logo.targetsize-\(size)\(suffix).png", size, size, size))
  }
}

try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
for (name, width, height, size) in images {
  let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)!
  rep.size = NSSize(width: width, height: height)
  NSGraphicsContext.saveGraphicsState()
  let context = NSGraphicsContext(bitmapImageRep: rep)!
  NSGraphicsContext.current = context
  context.imageInterpolation = .high
  NSColor.clear.set()
  NSRect(x: 0, y: 0, width: width, height: height).fill()
  icon.draw(
    in: NSRect(x: (width - size) / 2, y: (height - size) / 2, width: size, height: size),
    from: .zero, operation: .sourceOver, fraction: 1)
  NSGraphicsContext.restoreGraphicsState()
  try rep.representation(using: .png, properties: [:])!.write(to: outDir.appendingPathComponent(name))
}
print("wrote \(images.count) images to \(outDir.path)")
