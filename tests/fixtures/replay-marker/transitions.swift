// macOS QA helper: decode local MP4 frames with native AVFoundation. No GUI/ADB.
import AVFoundation
import AppKit
import CoreImage
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}
let args = CommandLine.arguments
if args.count != 5 { fail("Usage: swift transitions.swift INPUT.mp4 NEW_OUTPUT_DIRECTORY ROI_X ROI_Y (0..1)") }
guard let roiX = Double(args[3]), let roiY = Double(args[4]),
      roiX > 0, roiX < 1, roiY > 0, roiY < 1 else { fail("ROI must be inside the marker background, away from text/edges") }
let output = URL(fileURLWithPath: args[2], isDirectory: true)
let fm = FileManager.default
if fm.fileExists(atPath: output.path) { fail("Output already exists; refusing overwrite") }
try fm.createDirectory(at: output, withIntermediateDirectories: false,
                       attributes: [.posixPermissions: 0o700])
let asset = AVURLAsset(url: URL(fileURLWithPath: args[1]))
guard let track = asset.tracks(withMediaType: .video).first else { fail("No video track") }
let reader = try AVAssetReader(asset: asset)
let frames = AVAssetReaderTrackOutput(track: track, outputSettings: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
])
frames.alwaysCopiesSampleData = false
reader.add(frames)
guard reader.startReading() else { fail("Decode start failed: \(String(describing: reader.error))") }
let report = output.appendingPathComponent("frames.jsonl")
fm.createFile(atPath: report.path, contents: nil, attributes: [.posixPermissions: 0o600])
let handle = try FileHandle(forWritingTo: report)
let context = CIContext()
func image(_ pixel: CVPixelBuffer, _ name: String) throws {
    let ci = CIImage(cvPixelBuffer: pixel)
    guard let cg = context.createCGImage(ci, from: ci.extent) else { fail("Frame image conversion failed") }
    let bitmap = NSBitmapImageRep(cgImage: cg)
    guard let png = bitmap.representation(using: .png, properties: [:]) else { fail("PNG conversion failed") }
    try png.write(to: output.appendingPathComponent(name), options: .withoutOverwriting)
    try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: output.appendingPathComponent(name).path)
}
var count = 0
var transitions = 0
var unknown = 0
var priorTime: CMTime?
var priorPixel: CVPixelBuffer?
var priorColor: String?
while let sample = frames.copyNextSampleBuffer() {
    let pts = CMSampleBufferGetPresentationTimeStamp(sample)
    guard pts.isNumeric, pts.timescale > 0,
          priorTime == nil || CMTimeCompare(pts, priorTime!) > 0 else { fail("Non-numeric/non-increasing decoded PTS") }
    guard let pixel = CMSampleBufferGetImageBuffer(sample) else { fail("Decoded sample has no pixels") }
    CVPixelBufferLockBaseAddress(pixel, .readOnly)
    let w = CVPixelBufferGetWidth(pixel), h = CVPixelBufferGetHeight(pixel)
    guard w >= 5, h >= 5 else { fail("Frame is smaller than the 5x5 ROI") }
    let stride = CVPixelBufferGetBytesPerRow(pixel)
    guard let bytes = CVPixelBufferGetBaseAddress(pixel)?.assumingMemoryBound(to: UInt8.self) else { fail("No pixel buffer") }
    let x = min(w - 3, max(2, Int(roiX * Double(w))))
    let y = min(h - 3, max(2, Int(roiY * Double(h))))
    var rgb = [Double](repeating: 0, count: 3)
    for dy in -2...2 { for dx in -2...2 {
        let offset = (y + dy) * stride + (x + dx) * 4
        rgb[0] += Double(bytes[offset + 2]) / 25
        rgb[1] += Double(bytes[offset + 1]) / 25
        rgb[2] += Double(bytes[offset]) / 25
    } }
    CVPixelBufferUnlockBaseAddress(pixel, .readOnly)
    func distance(_ expected: [Double]) -> Double {
        sqrt(zip(rgb, expected).map { pow($0 - $1, 2) }.reduce(0, +))
    }
    let even = distance([0, 65, 120]), odd = distance([95, 0, 70])
    let color = min(even, odd) <= 50 ? (even < odd ? "even" : "odd") : "unknown"
    if color == "unknown" { unknown += 1 }
    let changed = priorColor != color
    var row: [String: Any] = ["sample_index": count, "pts_value": String(pts.value),
        "pts_timescale": pts.timescale, "decoded_pts_us": String(CMTimeConvertScale(pts, timescale: 1_000_000, method: .roundHalfAwayFromZero).value),
        "color": color, "rgb_mean": rgb, "transition": changed, "width": w, "height": h]
    if let previous = priorTime {
        row["previous_pts_value"] = String(previous.value)
        row["previous_pts_timescale"] = previous.timescale
    }
    if changed {
        // Color parity does not decode event numbers. Verify each exported pair manually.
        let currentName = String(format: "transition-%04d-first.png", transitions)
        try image(pixel, currentName)
        row["first_image"] = currentName
        if let previous = priorPixel {
            let name = String(format: "transition-%04d-before.png", transitions)
            try image(previous, name)
            row["before_image"] = name
        }
        transitions += 1
    }
    let json = try JSONSerialization.data(withJSONObject: row, options: [.sortedKeys])
    try handle.write(contentsOf: json + Data([10]))
    priorTime = pts; priorPixel = pixel; priorColor = color; count += 1
}
try handle.close()
guard reader.status == .completed else { fail("Decode failed: \(String(describing: reader.error))") }
if count == 0 { fail("No decoded frames") }
print("Decoded frames=\(count), color boundaries including initial=\(transitions), unknown frames=\(unknown). Event numbers/source/edit/clock mapping still require verification.")
