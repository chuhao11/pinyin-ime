// 生成输入法图标：纯黑底，白色"魔虚罗轮盘"（八根辐条、末端实心圆、半径约 0.45 处一个圆环），前面是"拼音"两个字。
// 用法：swift tools/make-icon.swift 输出目录 [样式]
//   样式 a：行楷，"拼"左上、"音"右下错落排开，右下角一枚朱红小印（默认）
//   样式 b：魏碑，两字横排居中
//   样式 c：楷体黑，两字竖排
// 字的周围留一圈黑边，压住轮盘的线，字和轮盘层次分明。小尺寸（< 48）只画轮盘和一个"拼"字，不然看不清。
import AppKit
import CoreGraphics
import CoreText
import Foundation

let args = CommandLine.arguments
let outDir = args.count > 1 ? args[1] : "assets/icon"
let style = args.count > 2 ? args[2] : "a"
try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)

func font(_ names: [String], _ size: CGFloat) -> CTFont {
    for name in names {
        let f = CTFontCreateWithName(name as CFString, size, nil)
        if (CTFontCopyPostScriptName(f) as String) == name { return f }
    }
    return CTFontCreateWithName("PingFangSC-Semibold" as CFString, size, nil)
}

let ink = CGColor(red: 0.97, green: 0.95, blue: 0.90, alpha: 1) // 略带暖色的白，像宣纸上的反白
let black = CGColor(red: 0, green: 0, blue: 0, alpha: 1)
let vermilion = CGColor(red: 0.80, green: 0.16, blue: 0.11, alpha: 1)

/// 画一个字：先用很粗的黑色描边"挖"出一圈空白，再填字。
func glyph(_ ctx: CGContext, _ text: String, _ f: CTFont, center: CGPoint, halo: CGFloat, color: CGColor = ink) {
    func line(_ stroke: Bool) -> CTLine {
        var attrs: [CFString: Any] = [kCTFontAttributeName: f]
        if stroke {
            attrs[kCTStrokeWidthAttributeName] = halo
            attrs[kCTStrokeColorAttributeName] = black
        } else {
            attrs[kCTForegroundColorAttributeName] = color
        }
        return CTLineCreateWithAttributedString(CFAttributedStringCreate(nil, text as CFString, attrs as CFDictionary))
    }
    let fill = line(false)
    let b = CTLineGetBoundsWithOptions(fill, .useGlyphPathBounds)
    let pos = CGPoint(x: center.x - b.midX, y: center.y - b.midY)
    if halo > 0 {
        ctx.textPosition = pos
        CTLineDraw(line(true), ctx)
    }
    ctx.textPosition = pos
    CTLineDraw(fill, ctx)
}

func wheel(_ ctx: CGContext, _ S: CGFloat, small: Bool) {
    let c = CGPoint(x: S / 2, y: S / 2)
    let spoke = S * 0.37
    let line = max(S * (small ? 0.04 : 0.012), 1.0)
    let dot = S * (small ? 0.055 : 0.038)
    let ring = spoke * 0.45
    // 整个轮盘画在一层里再整体半透明，辐条和圆点重叠的地方不会更亮
    ctx.setAlpha(small ? 0.5 : 0.88)
    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    ctx.setLineWidth(line)
    ctx.setLineCap(.round)
    for i in 0..<8 {
        let a = CGFloat(i) * .pi / 4 + .pi / 2
        let end = CGPoint(x: c.x + spoke * cos(a), y: c.y + spoke * sin(a))
        ctx.move(to: c)
        ctx.addLine(to: end)
        ctx.strokePath()
        ctx.fillEllipse(in: CGRect(x: end.x - dot, y: end.y - dot, width: dot * 2, height: dot * 2))
    }
    ctx.strokeEllipse(in: CGRect(x: c.x - ring, y: c.y - ring, width: ring * 2, height: ring * 2))
    ctx.endTransparencyLayer()
    ctx.setAlpha(1)
}

func seal(_ ctx: CGContext, _ S: CGFloat, at p: CGPoint) {
    let w = S * 0.105
    let r = CGRect(x: p.x - w / 2, y: p.y - w / 2, width: w, height: w)
    ctx.setFillColor(vermilion)
    ctx.addPath(CGPath(roundedRect: r, cornerWidth: w * 0.14, cornerHeight: w * 0.14, transform: nil))
    ctx.fillPath()
    // 印里反白一个"音"
    glyph(ctx, "音", font(["STKaitiSC-Black", "STKaitiSC-Bold"], w * 0.78), center: p, halo: 0,
          color: CGColor(red: 1, green: 0.96, blue: 0.92, alpha: 1))
}

func draw(size s: Int) -> Data {
    let S = CGFloat(s)
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(data: nil, width: s, height: s, bitsPerComponent: 8, bytesPerRow: 0, space: cs,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.setShouldAntialias(true)
    ctx.interpolationQuality = .high
    let small = s < 48

    // 纯黑圆角方块
    let inset = S * 0.02
    let rect = CGRect(x: inset, y: inset, width: S - 2 * inset, height: S - 2 * inset)
    ctx.addPath(CGPath(roundedRect: rect, cornerWidth: S * 0.2, cornerHeight: S * 0.2, transform: nil))
    ctx.setFillColor(black)
    ctx.fillPath()

    wheel(ctx, S, small: small)

    if small {
        glyph(ctx, "拼", font(["STXingkaiSC-Bold", "STKaitiSC-Black"], S * 0.78),
              center: CGPoint(x: S / 2, y: S / 2), halo: 12)
    } else {
        switch style {
        case "b":
            let f = font(["WeibeiSC-Bold"], S * 0.30)
            glyph(ctx, "拼", f, center: CGPoint(x: S * 0.33, y: S * 0.5), halo: 16)
            glyph(ctx, "音", f, center: CGPoint(x: S * 0.67, y: S * 0.5), halo: 16)
        case "c":
            let f = font(["STKaitiSC-Black"], S * 0.30)
            glyph(ctx, "拼", f, center: CGPoint(x: S * 0.5, y: S * 0.66), halo: 16)
            glyph(ctx, "音", f, center: CGPoint(x: S * 0.5, y: S * 0.34), halo: 16)
        default:
            // 行楷，"拼"大一点在左上，"音"小一点在右下，错落着压在轮盘上
            glyph(ctx, "拼", font(["STXingkaiSC-Bold"], S * 0.46), center: CGPoint(x: S * 0.38, y: S * 0.60), halo: 14)
            glyph(ctx, "音", font(["STXingkaiSC-Bold"], S * 0.36), center: CGPoint(x: S * 0.64, y: S * 0.35), halo: 14)
            seal(ctx, S, at: CGPoint(x: S * 0.80, y: S * 0.16))
        }
    }

    let img = ctx.makeImage()!
    return NSBitmapImageRep(cgImage: img).representation(using: .png, properties: [:])!
}

for s in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512] {
    let path = "\(outDir)/pinyin-\(s).png"
    try! draw(size: s).write(to: URL(fileURLWithPath: path))
}
print("生成 \(outDir)（样式 \(style)）")
