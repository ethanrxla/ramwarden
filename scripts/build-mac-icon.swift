// Render the existing pixel SVG on a site-inspired radar. No generated artwork.
import AppKit

final class Sprite: NSObject, XMLParserDelegate {
    var pixels: [(CGRect, NSColor)] = []
    func parser(_ parser: XMLParser, didStartElement element: String, namespaceURI: String?, qualifiedName: String?, attributes a: [String:String]) {
        guard element == "rect", let hex=a["fill"], hex.hasPrefix("#"), let rgb=UInt32(hex.dropFirst(),radix:16) else{return}
        pixels.append((CGRect(x:Double(a["x"] ?? "0")!,y:Double(a["y"] ?? "0")!,width:Double(a["width"] ?? "1")!,height:Double(a["height"] ?? "1")!),NSColor(srgbRed:CGFloat((rgb>>16)&255)/255,green:CGFloat((rgb>>8)&255)/255,blue:CGFloat(rgb&255)/255,alpha:1)))
    }
}
let root=URL(fileURLWithPath:CommandLine.arguments.count>1 ? CommandLine.arguments[1] : FileManager.default.currentDirectoryPath)
let sprite=Sprite(), parser=XMLParser(contentsOf:root.appendingPathComponent("web/assets/pets/ram.svg"))!
parser.delegate=sprite;guard parser.parse(),!sprite.pixels.isEmpty else{fatalError("Invalid canonical mascot")}
let bounds=sprite.pixels.map(\.0).reduce(CGRect.null){$0.union($1)}
func render(_ size:Int)->Data {
    let bitmap=NSBitmapImageRep(bitmapDataPlanes:nil,pixelsWide:size,pixelsHigh:size,bitsPerSample:8,samplesPerPixel:4,hasAlpha:true,isPlanar:false,colorSpaceName:.deviceRGB,bytesPerRow:0,bitsPerPixel:0)!
    NSGraphicsContext.saveGraphicsState();let graphics=NSGraphicsContext(bitmapImageRep:bitmap)!;NSGraphicsContext.current=graphics
    let ctx=graphics.cgContext;ctx.scaleBy(x:CGFloat(size)/1024,y:CGFloat(size)/1024)
    let tile=NSBezierPath(roundedRect:NSRect(x:64,y:64,width:896,height:896),xRadius:196,yRadius:196)
    NSColor(srgbRed:0.065,green:0.035,blue:0.065,alpha:1).setFill();tile.fill();tile.addClip()
    let red=NSColor(srgbRed:1,green:0.30,blue:0.37,alpha:1)
    for radius:CGFloat in [140,265,390,510] {
        red.withAlphaComponent(0.24).setStroke();let ring=NSBezierPath(ovalIn:NSRect(x:512-radius,y:512-radius,width:radius*2,height:radius*2));ring.lineWidth=5;ring.stroke()
    }
    red.withAlphaComponent(0.12).setStroke();let cross=NSBezierPath();cross.move(to:NSPoint(x:64,y:512));cross.line(to:NSPoint(x:960,y:512));cross.move(to:NSPoint(x:512,y:64));cross.line(to:NSPoint(x:512,y:960));cross.lineWidth=3;cross.stroke()
    // Soft static sweep; no animation or extra Dock CPU use.
    for angle in 0..<55 {
        let wedge=NSBezierPath();wedge.move(to:NSPoint(x:512,y:512));wedge.appendArc(withCenter:NSPoint(x:512,y:512),radius:590,startAngle:CGFloat(angle+25),endAngle:CGFloat(angle+26));wedge.close();red.withAlphaComponent(CGFloat(angle)/55*0.10).setFill();wedge.fill()
    }
    // Crop empty SVG margins and retain an integer pixel scale for crisp edges.
    let scale=floor(min(856/bounds.width,820/bounds.height))
    let left=(1024-bounds.width*scale)/2, bottom=(1024-bounds.height*scale)/2
    ctx.setShouldAntialias(false)
    for (rect,color) in sprite.pixels {
        color.setFill();NSBezierPath(rect:NSRect(x:left+(rect.minX-bounds.minX)*scale,y:bottom+(bounds.maxY-rect.maxY)*scale,width:rect.width*scale,height:rect.height*scale)).fill()
    }
    NSGraphicsContext.restoreGraphicsState();return bitmap.representation(using:.png,properties:[:])!
}
let brand=root.appendingPathComponent("assets/brand")
try render(1024).write(to:brand.appendingPathComponent("mac-app-icon.png"))
let temp=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
let iconset=temp.appendingPathComponent("RamWarden.iconset");try FileManager.default.createDirectory(at:iconset,withIntermediateDirectories:true)
defer{try? FileManager.default.removeItem(at:temp)}
for size in [16,32,128,256,512] {for factor in [1,2] {try render(size*factor).write(to:iconset.appendingPathComponent("icon_\(size)x\(size)\(factor==2 ? "@2x":"").png"))}}
let process=Process();process.executableURL=URL(fileURLWithPath:"/usr/bin/iconutil");process.arguments=["-c","icns",iconset.path,"-o",brand.appendingPathComponent("RamWarden.icns").path];try process.run();process.waitUntilExit();guard process.terminationStatus==0 else{fatalError("Icon packaging failed")}
print("Built Mac radar icon with maximal crisp mascot, without text")
