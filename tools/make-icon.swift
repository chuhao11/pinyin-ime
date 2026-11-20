// 生成输入法图标：深色圆角方块上，白色的"魔虚罗轮盘"（八根辐条、末端实心圆、中间一个圆环），前面是"拼音"两个字。
// 用法：swift tools/make-icon.swift assets/icon   （在 Mac 上运行，输出各种尺寸的 PNG）
import AppKit
import CoreGraphics
import CoreText
import Foundation

let outDir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "assets/icon"
try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)

func font(_ size: CGFloat) -> CTFont {
    for name in ["PingFangSC-Semibold", "PingFang SC Semibold", "STHeitiSC-Medium", "HiraginoSansGB-W6"] {
        let f = CTFontCreateWithName(name as CFString, size, nil)
        if (CTFontCopyFamilyName(f) as String).lowercased() != "lastresort" { return f }
    }
    return CTFontCreateUIFontForLanguage(.system, size, nil)!
}

func draw(size s: Int) -> Data {
    let S = CGFloat(s)
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(data: nil, width: s, height: s, bitsPerComponent: 8, bytesPerRow: 0, space: cs,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.setShouldAntialias(true)
    ctx.interpolationQuality = .high
    let small = s < 48

    // 深色圆角方块背景（上浅下深的渐变）
    let inset = S * 0.03
    let rect = CGRect(x: inset, y: inset, width: S - 2 * inset, height: S - 2 * inset)
    let bg = CGPath(roundedRect: rect, cornerWidth: S * 0.22, cornerHeight: S * 0.22, transform: nil)
    ctx.saveGState()
    ctx.addPath(bg)
    ctx.clip()
    let grad = CGGradient(colorsSpace: cs, colors: [
        CGColor(red: 0.20, green: 0.21, blue: 0.30, alpha: 1),
        CGColor(red: 0.07, green: 0.07, blue: 0.11, alpha: 1),
    ] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(grad, start: CGPoint(x: S / 2, y: S), end: CGPoint(x: S / 2, y: 0), options: [])
    ctx.restoreGState()

    // 魔虚罗轮盘：白色，八根辐条，末端实心圆，半径约 0.45 处一个圆环
    let c = CGPoint(x: S / 2, y: S / 2)
    let spoke = S * 0.355
    let line = max(S * (small ? 0.045 : 0.018), 1.0)
    let dot = S * (small ? 0.06 : 0.048)
    let ring = spoke * 0.45
    // 整个轮盘画在一层里再整体半透明，辐条和圆点重叠的地方不会更亮
    let white = CGColor(red: 1, green: 1, blue: 1, alpha: 1)
    ctx.setAlpha(small ? 0.55 : 0.92)
    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    ctx.setStrokeColor(white)
    ctx.setFillColor(white)
    ctx.setLineWidth(line)
    ctx.setLineCap(.round)
    for i in 0..<8 {
        let a = CGFloat(i) * .pi / 4
        let end = CGPoint(x: c.x + spoke * cos(a), y: c.y + spoke * sin(a))
        ctx.move(to: c)
        ctx.addLine(to: end)
        ctx.strokePath()
        ctx.fillEllipse(in: CGRect(x: end.x - dot, y: end.y - dot, width: dot * 2, height: dot * 2))
    }
    ctx.strokeEllipse(in: CGRect(x: c.x - ring, y: c.y - ring, width: ring * 2, height: ring * 2))
    ctx.endTransparencyLayer()
    ctx.setAlpha(1)

    // "拼音"：白字，深色描边和柔和的阴影，压在轮盘前面也看得清
    let fs = S * (small ? 0.40 : 0.215)
    let f = font(fs)
    let text = "拼音" as CFString
    func textLine(_ stroke: Bool) -> CTLine {
        var attrs: [CFString: Any] = [kCTFontAttributeName: f]
        if stroke {
            attrs[kCTStrokeWidthAttributeName] = small ? 10.0 : 4.0
            attrs[kCTStrokeColorAttributeName] = CGColor(red: 0.07, green: 0.07, blue: 0.11, alpha: 1)
        } else {
            attrs[kCTForegroundColorAttributeName] = CGColor(red: 1, green: 1, blue: 1, alpha: 1)
        }
        return CTLineCreateWithAttributedString(CFAttributedStringCreate(nil, text, attrs as CFDictionary))
    }
    let fill = textLine(false)
    let b = CTLineGetBoundsWithOptions(fill, .useGlyphPathBounds)
    let pos = CGPoint(x: c.x - b.midX, y: c.y - b.midY)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -S * 0.006), blur: S * 0.025,
                  color: CGColor(red: 0, green: 0, blue: 0, alpha: 0.75))
    ctx.textPosition = pos
    CTLineDraw(textLine(true), ctx)
    ctx.restoreGState()
    ctx.textPosition = pos
    CTLineDraw(fill, ctx)

    let img = ctx.makeImage()!
    let rep = NSBitmapImageRep(cgImage: img)
    return rep.representation(using: .png, properties: [:])!
}

for s in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512] {
    let path = "\(outDir)/pinyin-\(s).png"
    try! draw(size: s).write(to: URL(fileURLWithPath: path))
    print("生成 \(path)")
}
