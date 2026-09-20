import Foundation
import Vision
import CoreGraphics
import ImageIO

// macosocr —— PNG on stdin → JSON array on stdout
// 契约与 tools/winocr 一致：[{"text":"...","x":..,"y":..,"w":..,"h":..,"confidence":0.98}]
//
// 构建：swiftc -O -framework Vision -framework CoreGraphics -o macosocr macosocr.swift
// 运行：cat region.png | ./macosocr
//
// 完全离线。已知限制：小字号建议先放大（--upscale，默认 2）。

let DEFAULT_UPSCALE: CGFloat = 2.0
let MAX_UPSCALE: CGFloat = 8.0

struct OutBox: Codable {
    let text: String
    let x: Int
    let y: Int
    let w: Int
    let h: Int
    let confidence: Float
}

func isCJK(_ scalar: UnicodeScalar) -> Bool {
    let v = scalar.value
    return (0x4E00...0x9FFF).contains(v)
        || (0x3400...0x4DBF).contains(v)
        || (0x3000...0x303F).contains(v)
        || (0xFF00...0xFFEF).contains(v)
}

func collapseCJKSpaces(_ input: String) -> String {
    var out = String()
    let scalars = Array(input.unicodeScalars)
    var i = 0
    while i < scalars.count {
        let s = scalars[i]
        if s == " " {
            var j = i
            while j < scalars.count && scalars[j] == " " { j += 1 }
            let prevCJK = i > 0 && isCJK(scalars[i - 1])
            let nextCJK = j < scalars.count && isCJK(scalars[j])
            if !(prevCJK && nextCJK) {
                out.unicodeScalars.append(contentsOf: scalars[i..<j])
            }
            i = j
        } else {
            out.unicodeScalars.append(s)
            i += 1
        }
    }
    return out
}

func parseArgs() -> (upscale: CGFloat, language: String?) {
    var upscale = DEFAULT_UPSCALE
    var language: String? = nil
    var args = Array(CommandLine.arguments.dropFirst())
    var i = 0
    while i < args.count {
        let a = args[i]
        if a == "--upscale", i + 1 < args.count {
            guard let v = Double(args[i + 1]), v >= 1, v <= Double(MAX_UPSCALE) else {
                fputs("macosocr: 非法 --upscale\n", stderr)
                exit(2)
            }
            upscale = CGFloat(v)
            i += 2
        } else if a.hasPrefix("--upscale=") {
            let raw = String(a.dropFirst("--upscale=".count))
            guard let v = Double(raw), v >= 1, v <= Double(MAX_UPSCALE) else {
                fputs("macosocr: 非法 --upscale\n", stderr)
                exit(2)
            }
            upscale = CGFloat(v)
            i += 1
        } else if a.hasPrefix("-") {
            fputs("macosocr: 未知参数 \(a)\n", stderr)
            exit(2)
        } else if language == nil {
            language = a
            i += 1
        } else {
            fputs("macosocr: 多余参数 \(a)\n", stderr)
            exit(2)
        }
    }
    return (upscale, language)
}

func readStdin() -> Data {
    FileHandle.standardInput.readDataToEndOfFile()
}

func imageFromPNG(_ data: Data) -> CGImage? {
    guard let src = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
    return CGImageSourceCreateImageAtIndex(src, 0, nil)
}

func scaleImage(_ image: CGImage, factor: CGFloat) -> CGImage? {
    if factor <= 1.0001 { return image }
    let w = Int(CGFloat(image.width) * factor)
    let h = Int(CGFloat(image.height) * factor)
    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
        space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else { return nil }
    ctx.interpolationQuality = .high
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    return ctx.makeImage()
}

let (upscale, language) = parseArgs()
let png = readStdin()
guard !png.isEmpty, let base = imageFromPNG(png) else {
    fputs("macosocr: 标准输入不是有效 PNG\n", stderr)
    exit(1)
}
guard let image = scaleImage(base, factor: upscale) else {
    fputs("macosocr: 放大失败\n", stderr)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false
if let language {
    request.recognitionLanguages = [language]
} else {
    request.recognitionLanguages = ["zh-Hans", "zh-Hant", "en-US"]
}

let handler = VNImageRequestHandler(cgImage: image, options: [:])
do {
    try handler.perform([request])
} catch {
    fputs("macosocr: Vision 识别失败：\(error)\n", stderr)
    exit(1)
}

let inv = 1.0 / Double(upscale)
var boxes: [OutBox] = []
let imgW = Double(image.width)
let imgH = Double(image.height)
for obs in request.results ?? [] {
    guard let candidate = obs.topCandidates(1).first else { continue }
    let bb = obs.boundingBox // Vision: origin bottom-left, normalized
    let x = Int((bb.minX * imgW) * inv)
    let y = Int(((1.0 - bb.maxY) * imgH) * inv) // to top-left image coords
    let w = Int((bb.width * imgW) * inv)
    let h = Int((bb.height * imgH) * inv)
    let text = collapseCJKSpaces(candidate.string)
    if text.isEmpty { continue }
    boxes.append(OutBox(
        text: text, x: x, y: y, w: max(w, 0), h: max(h, 0),
        confidence: candidate.confidence
    ))
}

let encoder = JSONEncoder()
encoder.outputFormatting = []
let data = try! encoder.encode(boxes)
FileHandle.standardOutput.write(data)
