import AppKit

@main
struct PetSizingTests {
    static func main() {
        assert(PetSizing.normalized(.nan) == 3)
        assert(PetSizing.normalized(.infinity) == 3)
        assert(PetSizing.normalized(-100) == 2)
        assert(PetSizing.normalized(100) == 6)
        assert(PetSizing.normalized(3.7) == 4)
        assert(PetSizing.size("ram",scale:3) == NSSize(width:180,height:180))
        assert(PetSizing.size("threat",scale:3) == NSSize(width:160,height:180))
        let suiteName="RamWarden.PetSizingTests.\(UUID().uuidString)"
        let defaults=UserDefaults(suiteName:suiteName)!
        defer { defaults.removePersistentDomain(forName:suiteName) }
        assert(PetSizing.scale("ram",defaults:defaults) == 3)
        defaults.set(2.0,forKey:"ram.pixelScale")
        defaults.set(6.0,forKey:"threat.pixelScale")
        assert(PetSizing.scale("ram",defaults:defaults) == 2)
        assert(PetSizing.scale("threat",defaults:defaults) == 6)
        let bounds=NSRect(x:-1920,y:25,width:1920,height:1055)
        let old=NSRect(x:-1900,y:100,width:180,height:180)
        let smaller=PetSizing.resizedFrame(old,size:PetSizing.size("ram",scale:2),bounds:bounds)
        assert(smaller.midX == old.midX && smaller.minY == old.minY)
        for scale in 2...6 {
            let frame=PetSizing.resizedFrame(NSRect(x:-10,y:1050,width:180,height:180),size:PetSizing.size("ram",scale:scale),bounds:bounds)
            assert(bounds.contains(frame))
            let pixels=48*CGFloat(scale)
            assert(PetSizing.size("ram",scale:scale).height-36 == pixels)
        }
        for pet in [NSRect(x:-1900,y:1000,width:180,height:180), NSRect(x:-10,y:25,width:180,height:180)] {
            for above in [true,false] {
                let attached=PetSizing.attachedFrame(pet:pet,size:NSSize(width:304,height:440),bounds:bounds,preferAbove:above)
                assert(bounds.contains(attached))
            }
        }
        let topPet=NSRect(x:-1000,y:900,width:180,height:180)
        assert(PetSizing.attachedFrame(pet:topPet,size:NSSize(width:272,height:96),bounds:bounds,preferAbove:true).maxY < topPet.minY)
        let tiny=NSRect(x:0,y:0,width:90,height:90)
        assert(PetSizing.resizedFrame(old,size:PetSizing.size("threat",scale:6),bounds:tiny) == tiny)
        print("Pet sizing: scale bounds, independent persistence, crisp grid sizes and screen clamping passed.")
    }
}
