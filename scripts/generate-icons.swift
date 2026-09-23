// Render each native icon resolution directly from the vector artwork.
import AppKit
import CoreImage
import Foundation

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let assets = root.appendingPathComponent("assets/icons")
let variants: [(String, String, String?)] = [
  ("herdr-ui-icon-clean.svg", "herdr-worktree-1024.png", "Herdr"),
  ("herdr-icon-square-clean.svg", "herdr-square-worktree-1024.png", nil),
]
let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: temporary) }

func render(_ sourceName: String, pixels: Int) throws -> Data {
  let output = temporary.appendingPathComponent("render.png")
  let renderer = Process()
  renderer.executableURL = URL(fileURLWithPath: "/usr/bin/env")
  renderer.arguments = [
    "rsvg-convert", "--width", String(pixels), "--height", String(pixels),
    "--output", output.path, assets.appendingPathComponent(sourceName).path,
  ]
  try renderer.run()
  renderer.waitUntilExit()
  precondition(renderer.terminationStatus == 0, "rsvg-convert failed; install with brew install librsvg")
  let png = try Data(contentsOf: output)
  guard let image = NSBitmapImageRep(data: png),
    image.pixelsWide == pixels, image.pixelsHigh == pixels
  else {
    fatalError("Unexpected SVG render dimensions")
  }
  return png
}

let colorContext = CIContext()
func worktreePNG(_ source: Data) -> Data {
  let image = NSBitmapImageRep(data: source)!.cgImage!
  // Map luminance to a saturated red palette while retaining the original alpha
  // and shading at this resolution.
  let red = CIImage(cgImage: image).applyingFilter(
    "CIColorMatrix",
    parameters: [
      "inputRVector": CIVector(x: 0.1382, y: 0.4649, z: 0.0469, w: 0),
      "inputGVector": CIVector(x: 0.0255, y: 0.0858, z: 0.0087, w: 0),
      "inputBVector": CIVector(x: 0.0255, y: 0.0858, z: 0.0087, w: 0),
      "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 1),
      "inputBiasVector": CIVector(x: 0.25, y: 0.02, z: 0.035, w: 0),
    ])
  guard let redImage = colorContext.createCGImage(red, from: red.extent) else {
    fatalError("Unable to generate the red worktree icon")
  }
  return NSBitmapImageRep(cgImage: redImage).representation(using: .png, properties: [:])!
}

for (sourceName, redName, bundleName) in variants {
  let source = try render(sourceName, pixels: 1024)
  let redPNG = worktreePNG(source)
  let pngURL = assets.appendingPathComponent(sourceName).deletingPathExtension().appendingPathExtension("png")
  try source.write(to: pngURL)
  try redPNG.write(to: assets.appendingPathComponent(redName))

  guard let bundleName else { continue }
  for (name, isWorktree) in [(bundleName, false), ("\(bundleName)-worktree", true)] {
    let iconset = temporary.appendingPathComponent("\(name).iconset")
    try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
    for size in [16, 32, 128, 256, 512] {
      for scale in [1, 2] {
        let pixels = size * scale
        let rendered = try render(sourceName, pixels: pixels)
        let png = isWorktree ? worktreePNG(rendered) : rendered
        let suffix = scale == 2 ? "@2x" : ""
        try png.write(to: iconset.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
      }
    }
    let iconutil = Process()
    iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
    iconutil.arguments = [
      "-c", "icns", iconset.path, "-o", assets.appendingPathComponent("\(name).icns").path,
    ]
    try iconutil.run()
    iconutil.waitUntilExit()
    precondition(iconutil.terminationStatus == 0, "iconutil failed")
    print("Generated assets/icons/\(name).icns")
  }
}
