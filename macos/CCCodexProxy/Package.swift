// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "CCCodexProxy",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "CCCodexProxy", targets: ["CCCodexProxy"])
    ],
    targets: [
        .executableTarget(
            name: "CCCodexProxy",
            path: "Sources/CCCodexProxy"
        )
    ]
)

