// swift-tools-version: 6.3
import PackageDescription

let package = Package(
  name: "ConfigurationStorage",
  platforms: [.macOS(.v26)],
  products: [
    .library(name: "ConfigurationStorage", targets: ["ConfigurationStorage"]),
    .library(name: "ConfigurationStorageTestSupport", targets: ["ConfigurationStorageTestSupport"]),
  ],
  targets: [
    .binaryTarget(
      name: "CConfigurationStorage", path: "Artifacts/CConfigurationStorage.xcframework"),
    .target(name: "ConfigurationStorage", dependencies: ["CConfigurationStorage"]),
    .target(name: "ConfigurationStorageTestSupport"),
    .testTarget(
      name: "ConfigurationStorageTests",
      dependencies: [
        "CConfigurationStorage", "ConfigurationStorage", "ConfigurationStorageTestSupport",
      ]),
  ]
)
