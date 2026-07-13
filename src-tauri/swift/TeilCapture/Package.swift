// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "TeilCapture",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "TeilCapture", type: .static, targets: ["TeilCapture"])
    ],
    targets: [
        .target(name: "TeilCapture", path: "Sources/TeilCapture")
    ]
)
